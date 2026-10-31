//! Parser candidates (pulldown-cmark, comrak) over the whole vault: do they
//! agree on headings and wiki-links, and how fast is each.

use std::time::{Duration, Instant};

use comrak::nodes::NodeValue;
use pulldown_cmark::{Event, LinkType, Parser, Tag, TagEnd};

#[derive(Debug, PartialEq, Default)]
pub struct Outline {
    pub headings: Vec<(u8, String)>,
    pub links: Vec<String>,
}

pub fn pulldown(text: &str) -> Outline {
    let mut out = Outline::default();
    let mut heading: Option<(u8, String)> = None;
    for event in Parser::new_ext(text, crate::spec::options()) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                heading = Some((level as u8, String::new()))
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((l, t)) = heading.take() {
                    out.headings.push((l, t.trim().to_string()));
                }
            }
            Event::Text(t) | Event::Code(t) => {
                if let Some((_, h)) = heading.as_mut() {
                    h.push_str(&t);
                }
            }
            Event::Start(Tag::Link {
                link_type: LinkType::WikiLink { .. },
                dest_url,
                ..
            }) => {
                out.links.push(dest_url.to_string());
            }
            Event::Start(Tag::Image {
                link_type: LinkType::WikiLink { .. },
                dest_url,
                ..
            }) => {
                out.links.push(format!("!{dest_url}"));
            }
            _ => {}
        }
    }
    out
}

pub fn comrak(text: &str) -> Outline {
    let mut opts = comrak::Options::default();
    opts.extension.table = true;
    opts.extension.tasklist = true;
    opts.extension.strikethrough = true;
    opts.extension.front_matter_delimiter = Some("---".into());
    opts.extension.wikilinks_title_after_pipe = true;
    let arena = comrak::Arena::new();
    let root = comrak::parse_document(&arena, text, &opts);
    let mut out = Outline::default();
    for node in root.descendants() {
        match &node.data().value {
            NodeValue::Heading(h) => {
                let mut t = String::new();
                for d in node.descendants() {
                    match &d.data().value {
                        NodeValue::Text(s) => t.push_str(s),
                        NodeValue::Code(c) => t.push_str(&c.literal),
                        _ => {}
                    }
                }
                out.headings.push((h.level, t.trim().to_string()));
            }
            NodeValue::WikiLink(w) => {
                out.links.push(if w.embed {
                    format!("!{}", w.url)
                } else {
                    w.url.clone()
                });
            }
            _ => {}
        }
    }
    out
}

pub struct Comparison {
    pub files: usize,
    pub bytes: usize,
    pub same_headings: usize,
    pub same_links: usize,
    pub pulldown_time: Duration,
    pub comrak_time: Duration,
    pub diffs: Vec<String>,
}

pub fn run(docs: &[(String, String)]) -> Comparison {
    let mut c = Comparison {
        files: docs.len(),
        bytes: docs.iter().map(|(_, t)| t.len()).sum(),
        same_headings: 0,
        same_links: 0,
        pulldown_time: Duration::ZERO,
        comrak_time: Duration::ZERO,
        diffs: vec![],
    };
    for (path, text) in docs {
        let t = Instant::now();
        let a = pulldown(text);
        c.pulldown_time += t.elapsed();
        let t = Instant::now();
        let b = comrak(text);
        c.comrak_time += t.elapsed();
        if a.headings == b.headings {
            c.same_headings += 1;
        } else if c.diffs.len() < 12 {
            let (x, y) = first_diff(&a.headings, &b.headings);
            c.diffs
                .push(format!("{path} heading: pulldown={x:?} comrak={y:?}"));
        }
        if a.links == b.links {
            c.same_links += 1;
        } else if c.diffs.len() < 24 {
            let (x, y) = first_diff(&a.links, &b.links);
            c.diffs
                .push(format!("{path} link: pulldown={x:?} comrak={y:?}"));
        }
    }
    c
}

fn first_diff<T: PartialEq + Clone>(a: &[T], b: &[T]) -> (Option<T>, Option<T>) {
    let i = a
        .iter()
        .zip(b)
        .position(|(x, y)| x != y)
        .unwrap_or(a.len().min(b.len()));
    (a.get(i).cloned(), b.get(i).cloned())
}
