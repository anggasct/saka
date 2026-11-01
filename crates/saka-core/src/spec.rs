use std::collections::{BTreeMap, VecDeque};

use serde_json::{Value, json};
use uuid::Uuid;

use crate::Ctx;
use crate::event::Event;
use crate::ids::{ProjectId, RepoId, SpecId, SpecRevisionId, WorkItemId};
use crate::model::{DependsOn, RevisionView, Spec, SpecDependency, WorkItemDependency};
use crate::rejection::Rejection;
use crate::time::Timestamp;
use crate::vocab::{SpecOrigin, SpecState};

#[derive(Debug, Clone, PartialEq)]
pub struct NewRevision {
    pub source_commit: String,
    pub content_hash: String,
    pub lint_ok: bool,
    pub issue_codes: Vec<String>,
    pub title: String,
    pub parser_version: u32,
    pub priority: i64,
}

pub fn record_revision(
    existing: Option<&Spec>,
    project_id: ProjectId,
    path: &str,
    slug: &str,
    revision: NewRevision,
    known: Option<SpecRevisionId>,
    ctx: &mut Ctx<'_>,
) -> Vec<Event> {
    let mut events = Vec::new();
    let spec_id = match existing {
        Some(spec) => {
            if known == Some(spec.latest_revision_id) {
                return events;
            }
            spec.id
        }
        None => {
            let id = SpecId::new(ctx.ids);
            events.push(Event::SpecRegistered {
                spec_id: id,
                project_id,
                path: path.to_string(),
                slug: slug.to_string(),
            });
            id
        }
    };
    events.push(Event::SpecRevisionRecorded {
        spec_id,
        revision_id: known.unwrap_or_else(|| SpecRevisionId::new(ctx.ids)),
        source_commit: revision.source_commit,
        content_hash: revision.content_hash,
        lint_ok: revision.lint_ok,
        issue_codes: revision.issue_codes,
        title: revision.title,
        reused: known.is_some(),
        parser_version: revision.parser_version,
        priority: revision.priority,
    });
    events
}

pub struct Approval<'a> {
    pub if_match: SpecRevisionId,
    pub reason: Option<String>,
    pub default_repo: RepoId,
    pub parser_version: u32,
    pub depends_on_line: u32,
    pub project_specs: &'a [Spec],
    pub project_graph: &'a [SpecDependency],
}

pub fn approve(
    spec: &Spec,
    latest: &RevisionView,
    approval: Approval<'_>,
    ctx: &mut Ctx<'_>,
) -> Result<Vec<Event>, Rejection> {
    if approval.if_match != spec.latest_revision_id || latest.id != spec.latest_revision_id {
        return Err(Rejection::RevisionStale);
    }
    if latest.parser_version < approval.parser_version {
        return Err(Rejection::SpecReparseRequired {
            parser_version: latest.parser_version,
            current_parser_version: approval.parser_version,
        });
    }
    if !latest.lint_ok {
        return Err(Rejection::SpecNotReady {
            issues: latest.issues.clone(),
        });
    }
    match spec.state {
        SpecState::Ready => {}
        SpecState::Approved => return Err(Rejection::SpecAlreadyApproved),
        other => {
            return Err(Rejection::InvalidState {
                state: other.as_str().to_string(),
            });
        }
    }
    let (depends_on, issues) = resolve_dependencies(spec, latest, &approval);
    if !issues.is_empty() {
        return Err(Rejection::SpecNotReady {
            issues: Value::Array(issues),
        });
    }
    let mut events = vec![Event::SpecApproved {
        spec_id: spec.id,
        revision_id: latest.id,
        reason: approval.reason,
        priority: latest.priority,
        depends_on,
    }];
    for step in &latest.steps {
        let key = format!("{}/s{}", spec.slug, step.index);
        events.push(Event::WorkItemCreated {
            work_item_id: WorkItemId::new(ctx.ids),
            project_id: spec.project_id,
            spec_id: spec.id,
            spec_revision_id: latest.id,
            step_id: step.id.clone(),
            step_index: step.index,
            branch: format!("saka/{key}"),
            task_key: key,
            work: step.work.clone(),
            repo_id: approval.default_repo,
            priority: latest.priority,
        });
    }
    Ok(events)
}

fn dependency_issue(code: &str, line: u32, params: Value) -> Value {
    json!({ "code": code, "severity": "error", "line": line, "params": params })
}

fn resolve_dependencies(
    spec: &Spec,
    latest: &RevisionView,
    approval: &Approval<'_>,
) -> (Vec<DependsOn>, Vec<Value>) {
    let line = approval.depends_on_line;
    let mut resolved = Vec::new();
    let mut issues = Vec::new();
    for slug in &latest.depends_on {
        let target = approval
            .project_specs
            .iter()
            .find(|s| s.project_id == spec.project_id && s.slug == *slug && s.id != spec.id);
        match target {
            None => issues.push(dependency_issue(
                "depends_on_unknown",
                line,
                json!({ "slug": slug }),
            )),
            Some(t) if t.state == SpecState::Withdrawn => issues.push(dependency_issue(
                "depends_on_withdrawn",
                line,
                json!({ "slug": slug }),
            )),
            Some(t) => resolved.push(DependsOn {
                slug: slug.clone(),
                spec_id: t.id,
            }),
        }
    }
    for dep in &resolved {
        if let Some(path) = path_back(dep.spec_id, spec.id, approval.project_graph) {
            let mut cycle = vec![spec.slug.clone(), dep.slug.clone()];
            cycle.extend(path);
            issues.push(dependency_issue(
                "depends_on_cycle",
                line,
                json!({ "cycle": cycle }),
            ));
        }
    }
    (resolved, issues)
}

// Shortest chain of approved dependencies from `from` to `to`, as the slugs after `from`.
fn path_back(from: SpecId, to: SpecId, graph: &[SpecDependency]) -> Option<Vec<String>> {
    let mut edges: BTreeMap<SpecId, Vec<&SpecDependency>> = BTreeMap::new();
    for edge in graph {
        edges.entry(edge.spec_id).or_default().push(edge);
    }
    for list in edges.values_mut() {
        list.sort_by(|a, b| a.depends_on_slug.cmp(&b.depends_on_slug));
    }
    let mut came_from: BTreeMap<SpecId, &SpecDependency> = BTreeMap::new();
    let mut queue = VecDeque::from([from]);
    while let Some(node) = queue.pop_front() {
        if node == to {
            let mut path = Vec::new();
            let mut at = node;
            while let Some(edge) = came_from.get(&at) {
                path.push(edge.depends_on_slug.clone());
                at = edge.spec_id;
            }
            path.reverse();
            return Some(path);
        }
        for edge in edges.get(&node).into_iter().flatten() {
            let next = edge.depends_on_spec_id;
            if next != from && !came_from.contains_key(&next) {
                came_from.insert(next, edge);
                queue.push_back(next);
            }
        }
    }
    None
}

pub fn approval_graph(
    spec: &Spec,
    latest: &RevisionView,
    events: &[Event],
) -> (Vec<SpecDependency>, Vec<WorkItemDependency>) {
    let mut specs = Vec::new();
    let mut items: BTreeMap<&str, WorkItemId> = BTreeMap::new();
    for event in events {
        match event {
            Event::SpecApproved {
                spec_id,
                revision_id,
                depends_on,
                ..
            } => specs.extend(depends_on.iter().map(|d| (*spec_id, *revision_id, d))),
            Event::WorkItemCreated {
                work_item_id,
                step_id,
                ..
            } => {
                items.insert(step_id.as_str(), *work_item_id);
            }
            _ => {}
        }
    }
    let spec_rows = specs
        .into_iter()
        .map(|(spec_id, revision_id, d)| SpecDependency {
            spec_id,
            depends_on_spec_id: d.spec_id,
            depends_on_slug: d.slug.clone(),
            revision_id,
            project_id: spec.project_id,
        })
        .collect();
    let mut item_rows = Vec::new();
    for step in &latest.steps {
        let Some(work_item_id) = items.get(step.id.as_str()) else {
            continue;
        };
        for after in &step.after {
            if let Some(depends_on) = items.get(after.as_str()) {
                item_rows.push(WorkItemDependency {
                    work_item_id: *work_item_id,
                    depends_on_work_item_id: *depends_on,
                    project_id: spec.project_id,
                });
            }
        }
    }
    (spec_rows, item_rows)
}

pub fn ship(spec: &Spec, all_tasks_done: bool) -> Result<Vec<Event>, Rejection> {
    match (spec.state, spec.approved_revision_id) {
        (SpecState::Approved, Some(revision_id)) if all_tasks_done => {
            Ok(vec![Event::SpecShipped {
                spec_id: spec.id,
                revision_id,
            }])
        }
        (state, _) => Err(Rejection::InvalidState {
            state: state.as_str().to_string(),
        }),
    }
}

pub fn evolve(spec: Option<Spec>, event: &Event, now: Timestamp) -> Option<Spec> {
    match (spec, event) {
        (
            None,
            Event::SpecRegistered {
                spec_id,
                project_id,
                path,
                slug,
            },
        ) => Some(Spec {
            id: *spec_id,
            project_id: *project_id,
            path: path.clone(),
            slug: slug.clone(),
            title: String::new(),
            state: SpecState::Draft,
            origin: SpecOrigin::Manual,
            priority: 0,
            latest_revision_id: SpecRevisionId(Uuid::nil()),
            approved_revision_id: None,
            approved_at: None,
            shipped_at: None,
            created_at: now,
            updated_at: now,
        }),
        (
            Some(mut s),
            Event::SpecRevisionRecorded {
                revision_id,
                lint_ok,
                title,
                priority,
                ..
            },
        ) => {
            s.latest_revision_id = *revision_id;
            s.title = title.clone();
            if matches!(s.state, SpecState::Draft | SpecState::Ready) {
                s.priority = *priority;
                s.state = if *lint_ok {
                    SpecState::Ready
                } else {
                    SpecState::Draft
                };
            }
            s.updated_at = now;
            Some(s)
        }
        (
            Some(mut s),
            Event::SpecApproved {
                revision_id,
                priority,
                ..
            },
        ) => {
            s.state = SpecState::Approved;
            s.priority = *priority;
            s.approved_revision_id = Some(*revision_id);
            s.approved_at = Some(now);
            s.updated_at = now;
            Some(s)
        }
        (Some(mut s), Event::SpecShipped { .. }) => {
            s.state = SpecState::Shipped;
            s.shipped_at = Some(now);
            s.updated_at = now;
            Some(s)
        }
        (spec, _) => spec,
    }
}
