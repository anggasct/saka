use std::io::{BufRead, Write};

use serde_json::json;

use super::InitError;

pub const MAX_ATTEMPTS: usize = 3;

pub trait Prompter {
    fn ask(&mut self, question: &str) -> Option<String>;
}

pub struct Terminal;

impl Prompter for Terminal {
    fn ask(&mut self, question: &str) -> Option<String> {
        let mut stderr = std::io::stderr();
        let _ = write!(stderr, "{question} ");
        let _ = stderr.flush();
        let mut line = String::new();
        match std::io::stdin().lock().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line.trim_end_matches(['\n', '\r']).to_string()),
        }
    }
}

fn invalid(question: &str) -> InitError {
    InitError::usage("prompt_invalid", json!({ "question": question }))
}

pub fn ask<T>(
    prompter: &mut dyn Prompter,
    question: &str,
    default: Option<T>,
    parse: impl Fn(&str) -> Option<T>,
) -> Result<T, InitError> {
    let mut default = default;
    for _ in 0..MAX_ATTEMPTS {
        let answer = prompter.ask(question).ok_or_else(|| invalid(question))?;
        let answer = answer.trim();
        if answer.is_empty() {
            if let Some(value) = default.take() {
                return Ok(value);
            }
            continue;
        }
        if let Some(value) = parse(answer) {
            return Ok(value);
        }
    }
    Err(invalid(question))
}

pub fn confirm(prompter: &mut dyn Prompter, question: &str) -> Result<bool, InitError> {
    ask(prompter, question, Some(true), |a| {
        match a.to_ascii_lowercase().as_str() {
            "y" | "yes" => Some(true),
            "n" | "no" => Some(false),
            _ => None,
        }
    })
}

#[cfg(test)]
pub struct Scripted {
    pub answers: std::collections::VecDeque<String>,
    pub asked: Vec<String>,
}

#[cfg(test)]
impl Scripted {
    pub fn new(answers: &[&str]) -> Scripted {
        Scripted {
            answers: answers.iter().map(|a| (*a).to_string()).collect(),
            asked: Vec::new(),
        }
    }
}

#[cfg(test)]
impl Prompter for Scripted {
    fn ask(&mut self, question: &str) -> Option<String> {
        self.asked.push(question.to_string());
        self.answers.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_answer_takes_the_default_and_bad_answers_are_asked_again() {
        let mut p = Scripted::new(&["", "maybe", "n"]);
        assert!(confirm(&mut p, "Write x? [Y/n]").unwrap());
        assert!(!confirm(&mut p, "Write y? [Y/n]").unwrap());
        assert_eq!(
            p.asked,
            ["Write x? [Y/n]", "Write y? [Y/n]", "Write y? [Y/n]"]
        );
    }

    #[test]
    fn three_invalid_answers_or_the_end_of_input_refuse_with_exit_two() {
        let mut p = Scripted::new(&["x", "y?", "nope", "y"]);
        let e = confirm(&mut p, "Write x? [Y/n]").unwrap_err();
        assert_eq!((e.exit, e.code), (2, "prompt_invalid"));
        assert_eq!(p.asked.len(), MAX_ATTEMPTS);
        let mut empty = Scripted::new(&[]);
        let e = ask(&mut empty, "Name:", None::<String>, |a| Some(a.to_string())).unwrap_err();
        assert_eq!(e.code, "prompt_invalid");
        let mut blank = Scripted::new(&["", "", ""]);
        let e = ask(&mut blank, "Path:", None::<String>, |a| Some(a.to_string())).unwrap_err();
        assert_eq!(e.code, "prompt_invalid");
    }
}
