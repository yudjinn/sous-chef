//! The python subset of `eval::pyexpr` against the real helpers.
//!
//! `tests/fixtures/pyexpr/oracle.tsv` was written by `tests/fixtures/pyexpr/oracle.py`, which
//! runs every expression with BitBake 2.18's `lib/bb` and OE-core Wrynose's `meta/lib`
//! (`bb.utils`, `oe.utils`, `oe.types` imported from those trees, `eval(compile(code.strip(),
//! ..., "eval"))` with a `d` whose `getVar` returns the values given) for every pair of values
//! of `V` and `W` (`null` = unset), and records `str(result)` (a JSON string) or
//! `ERROR:<exception>`. The
//! other variables are fixed: `DISTRO_FEATURES = "a x11 opengl"`, `MLPREFIX = "lib32-"`,
//! `X_DEFAULTS = "c a b a"`, `X_OPTED_OUT = "b"`; the flag `V[f]` is `V`.
//!
//! The rule checked: whenever sous-chef evaluates an expression, the result is python's;
//! when python raises, sous-chef does not evaluate. Not evaluating where python succeeds is
//! allowed (outside the subset) and counted.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;

use sous_chef::eval::pyexpr::{self, Env, Stop};

struct Vars {
    vars: HashMap<String, String>,
    flags: HashMap<String, String>,
}

impl Env for Vars {
    fn get_var(&mut self, name: &str) -> Result<Option<String>, Stop> {
        Ok(self.vars.get(name).cloned())
    }
    fn get_var_flag(&mut self, name: &str, flag: &str) -> Result<Option<String>, Stop> {
        Ok(self.flags.get(&format!("{name}[{flag}]")).cloned())
    }
    fn expand(&mut self, text: &str) -> Result<String, Stop> {
        // `${NAME}` replaced by its value, left when unset (the oracle's `d.expand`).
        let mut out = String::new();
        let mut rest = text;
        while let Some(p) = rest.find("${") {
            out.push_str(&rest[..p]);
            let after = &rest[p + 2..];
            match after.find('}') {
                Some(e) => {
                    let name = &after[..e];
                    match self.vars.get(name) {
                        Some(v) => out.push_str(v),
                        None => out.push_str(&rest[p..p + 3 + e]),
                    }
                    rest = &after[e + 1..];
                }
                None => {
                    out.push_str(&rest[p..]);
                    rest = "";
                }
            }
        }
        out.push_str(rest);
        Ok(out)
    }
}

fn json_opt(s: &str) -> Option<String> {
    if s == "null" {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(s).unwrap();
    Some(v.as_str().unwrap().to_string())
}

#[test]
fn subset_agrees_with_bitbake_and_oe_core() {
    let text = include_str!("fixtures/pyexpr/oracle.tsv");
    let mut lines = text.lines();
    let vs: Vec<Option<String>> = lines
        .next()
        .unwrap()
        .split('\t')
        .skip(1)
        .map(json_opt)
        .collect();
    let ws: Vec<Option<String>> = lines
        .next()
        .unwrap()
        .split('\t')
        .skip(1)
        .map(json_opt)
        .collect();
    assert_eq!(vs.len(), ws.len());
    let (mut agreed, mut skipped, mut python_ok, mut errors_left) = (0, 0, 0, 0);
    let mut wrong = Vec::new();
    let mut never: Vec<&str> = Vec::new();
    for line in lines {
        let mut cols = line.split('\t');
        let code = cols.next().unwrap();
        let results: Vec<&str> = cols.collect();
        assert_eq!(results.len(), vs.len(), "{code}");
        let mut evaluated_any = false;
        for ((v, w), expected) in vs.iter().zip(&ws).zip(&results) {
            let mut vars: HashMap<String, String> = [
                ("DISTRO_FEATURES", "a x11 opengl"),
                ("MLPREFIX", "lib32-"),
                ("X_DEFAULTS", "c a b a"),
                ("X_OPTED_OUT", "b"),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
            let mut flags = HashMap::new();
            if let Some(v) = v {
                vars.insert("V".into(), v.clone());
                flags.insert("V[f]".to_string(), v.clone());
            }
            if let Some(w) = w {
                vars.insert("W".into(), w.clone());
            }
            let mut env = Vars { vars, flags };
            let got = pyexpr::parse(code)
                .and_then(|e| pyexpr::eval(&e, &mut env))
                .and_then(|v| pyexpr::to_text(&v));
            let python_error = expected.starts_with("ERROR:");
            let expected = if python_error {
                expected.to_string()
            } else {
                json_opt(expected).unwrap()
            };
            match (&got, python_error) {
                (Ok(g), false) if *g == expected => {
                    agreed += 1;
                    python_ok += 1;
                    evaluated_any = true;
                }
                (Ok(g), _) => wrong.push(format!(
                    "{code} V={v:?} W={w:?}: python {expected:?}, sous-chef {g:?}"
                )),
                (Err(_), false) => {
                    skipped += 1;
                    python_ok += 1;
                }
                (Err(_), true) => errors_left += 1,
            }
        }
        if !evaluated_any {
            never.push(code);
        }
    }
    assert!(
        wrong.is_empty(),
        "{} wrong:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
    eprintln!(
        "agreed {agreed}, not evaluated where python succeeds {skipped} (of {python_ok}), python errors left unevaluated {errors_left}"
    );
    eprintln!("never evaluated: {never:?}");
    // Nearly every expression of the oracle is in the subset.
    assert!(agreed * 10 >= python_ok * 9, "{agreed} of {python_ok}");
}
