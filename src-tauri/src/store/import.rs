//! `servers.json` from another machine or an agent: parsed and checked here, the UI only diffs
//! the result with its list. Format of spec section 11: `{ version, groups: [{ name, servers }] }`
//! or a bare `{ servers: [...] }`.

use std::collections::HashSet;

use serde::Serialize;
use serde_json::{Map, Value};

use super::servers::ServerUser;

/// Bigger files are not a server list.
pub const MAX_IMPORT: usize = 2 * 1024 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Parsed {
    pub code: String,
    pub desc: String,
    pub host: String,
    pub port: u16,
    pub users: Vec<ServerUser>,
    pub def_user: String,
    /// Empty: no group in the file, the UI names it in its language.
    pub group: String,
}

/// What is wrong with the file; the UI words it. `server` is the code as written in the file.
#[derive(Debug, PartialEq, Serialize)]
#[serde(tag = "code", rename_all = "SCREAMING_SNAKE_CASE")]
#[allow(clippy::enum_variant_names, reason = "the names are the IMPORT_* codes the UI gets")]
pub enum ImportError {
    ImportBig,
    ImportJson { reason: String },
    ImportVersion { version: String },
    ImportNoGroups,
    ImportCode { server: String },
    ImportHost { server: String },
    ImportPort { server: String },
    ImportUser { server: String, user: String },
    ImportKey { server: String, key: String },
    ImportDup { server: String },
}

type Res<T> = Result<T, ImportError>;

/// Same rule as the server card.
pub fn valid_code(code: &str) -> bool {
    let mut it = code.chars();
    it.next().is_some_and(|c| c.is_ascii_alphanumeric()) && it.all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// A key is a file name in ~/.ssh, never a path.
fn valid_key(key: &str) -> bool {
    !key.is_empty() && key != "." && key != ".." && !key.contains(['/', '\\', ':']) && !key.chars().any(char::is_control)
}

fn text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.trim().to_owned(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// Digits only, as the UI checks it: `22.0` is a whole number and passes, `"+22"` does not.
fn port(v: Option<&Value>) -> Option<u16> {
    let p = match v? {
        Value::Number(n) => n.as_f64().filter(|f| f.fract() == 0.0 && (1.0..=65535.0).contains(f))? as u16,
        Value::String(s) => {
            let s = s.trim();
            if s.is_empty() || s.len() > 5 || !s.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            s.parse().ok()?
        }
        _ => return None,
    };
    (p > 0).then_some(p)
}

pub fn parse(raw: &str) -> Res<Vec<Parsed>> {
    if raw.len() > MAX_IMPORT {
        return Err(ImportError::ImportBig);
    }
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let json: Value = serde_json::from_str(raw).map_err(|e| ImportError::ImportJson { reason: e.to_string() })?;
    let Some(root) = json.as_object() else { return Err(ImportError::ImportNoGroups) };
    match root.get("version") {
        None => {}
        // `1.0` passes too: the UI's JSON cannot tell it from `1`.
        Some(v) if v.as_f64() == Some(1.0) => {}
        Some(v) => return Err(ImportError::ImportVersion { version: text(Some(v)).chars().take(20).collect() }),
    }
    let groups: Vec<(String, &Vec<Value>)> = if let Some(gs) = root.get("groups").and_then(Value::as_array) {
        gs.iter()
            .filter_map(Value::as_object)
            .map(|g| (text(g.get("name")), g.get("servers").and_then(Value::as_array)))
            .filter_map(|(n, s)| Some((n, s?)))
            .collect()
    } else if let Some(s) = root.get("servers").and_then(Value::as_array) {
        vec![(String::new(), s)]
    } else {
        return Err(ImportError::ImportNoGroups);
    };
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (group, list) in groups {
        for s in list {
            let empty = Map::new();
            let s = s.as_object().unwrap_or(&empty);
            let p = server(s, &group)?;
            if !seen.insert(p.code.clone()) {
                return Err(ImportError::ImportDup { server: p.code });
            }
            out.push(p);
        }
    }
    Ok(out)
}

fn server(s: &Map<String, Value>, group: &str) -> Res<Parsed> {
    let code = text(s.get("code"));
    if !valid_code(&code) {
        return Err(ImportError::ImportCode { server: code });
    }
    let host = text(s.get("host"));
    if host.is_empty() || host.contains(char::is_whitespace) {
        return Err(ImportError::ImportHost { server: code });
    }
    let port = match s.get("port") {
        None | Some(Value::Null) => 22,
        v => port(v).ok_or_else(|| ImportError::ImportPort { server: code.clone() })?,
    };
    let mut users = Vec::new();
    let mut def_user = None;
    for u in s.get("users").and_then(Value::as_array).into_iter().flatten() {
        let empty = Map::new();
        let u = u.as_object().unwrap_or(&empty);
        let name = text(u.get("name"));
        if !crate::fs::valid_account(&name) || users.iter().any(|x: &ServerUser| x.name == name) {
            return Err(ImportError::ImportUser { server: code, user: name });
        }
        let key = match text(u.get("key")) {
            k if k.is_empty() || k == "auto" => "auto".to_owned(),
            k if valid_key(&k) => k,
            k => return Err(ImportError::ImportKey { server: code, key: k }),
        };
        if def_user.is_none() && u.get("default").and_then(Value::as_bool) == Some(true) {
            def_user = Some(name.clone());
        }
        users.push(ServerUser { name, key });
    }
    if users.is_empty() {
        users.push(ServerUser { name: "root".into(), key: "auto".into() });
    }
    let def_user = def_user.unwrap_or_else(|| users[0].name.clone());
    // A server may name its group itself, as in a bare `servers` list.
    let own = text(s.get("group"));
    Ok(Parsed {
        code,
        desc: text(s.get("description")),
        host,
        port,
        users,
        def_user,
        group: if own.is_empty() { group.to_owned() } else { own },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = r#"{ "version": 1,
      "groups": [
        { "name": "Widgets · Selectel",
          "servers": [
            { "code": "sse7", "description": "Web", "host": "203.0.113.17", "port": 22,
              "users": [ { "name": "root", "default": true },
                         { "name": "deploy", "key": "deploy_ed25519" } ] } ] } ] }"#;

    #[test]
    fn spec_example() {
        let l = parse(SPEC).unwrap();
        assert_eq!(l.len(), 1);
        let s = &l[0];
        assert_eq!((s.code.as_str(), s.host.as_str(), s.port, s.group.as_str()), ("sse7", "203.0.113.17", 22, "Widgets · Selectel"));
        assert_eq!(s.def_user, "root");
        assert_eq!(s.users[0].key, "auto");
        assert_eq!(s.users[1].key, "deploy_ed25519");
    }

    #[test]
    fn bare_servers_and_defaults() {
        let l = parse(r#"{ "servers": [ { "code": "a1", "host": "h", "group": "G" }, { "code": "b", "host": "h2", "port": "2222",
            "users": [ { "name": "x" }, { "name": "y", "default": true } ] } ] }"#).unwrap();
        assert_eq!((l[0].port, l[0].users[0].name.as_str(), l[0].def_user.as_str(), l[0].group.as_str()), (22, "root", "root", "G"));
        assert_eq!((l[1].port, l[1].def_user.as_str(), l[1].group.as_str()), (2222, "y", ""));
        assert!(parse("\u{feff}{\"servers\":[]}").unwrap().is_empty());
    }

    #[test]
    fn rejects() {
        use ImportError::*;
        assert!(matches!(parse("{"), Err(ImportJson { .. })));
        assert_eq!(parse("[]").unwrap_err(), ImportNoGroups);
        assert_eq!(parse(r#"{"version":2,"groups":[]}"#).unwrap_err(), ImportVersion { version: "2".into() });
        assert_eq!(parse(r#"{"version":"1","groups":[]}"#).unwrap_err(), ImportVersion { version: "1".into() });
        assert_eq!(parse(r#"{"foo":1}"#).unwrap_err(), ImportNoGroups);
        let one = |s: &str| parse(&format!(r#"{{"servers":[{s}]}}"#)).unwrap_err();
        assert_eq!(one(r#"{"host":"h"}"#), ImportCode { server: "".into() });
        assert_eq!(one(r#"{"code":"-a","host":"h"}"#), ImportCode { server: "-a".into() });
        assert_eq!(one(r#"{"code":"a b","host":"h"}"#), ImportCode { server: "a b".into() });
        assert_eq!(one(r#"{"code":"a"}"#), ImportHost { server: "a".into() });
        assert_eq!(one(r#"{"code":"a","host":"h","port":0}"#), ImportPort { server: "a".into() });
        assert_eq!(one(r#"{"code":"a","host":"h","port":70000}"#), ImportPort { server: "a".into() });
        assert_eq!(one(r#"{"code":"a","host":"h","port":"+22"}"#), ImportPort { server: "a".into() });
        assert_eq!(one(r#"{"code":"a","host":"h","port":22.5}"#), ImportPort { server: "a".into() });
        // As the UI reads them: whole floats are numbers.
        let l = parse(r#"{"version":1.0,"servers":[{"code":"a","host":"h","port":22.0}]}"#).unwrap();
        assert_eq!(l[0].port, 22);
        assert_eq!(one(r#"{"code":"a","host":"h","users":[{"name":"r t"}]}"#), ImportUser { server: "a".into(), user: "r t".into() });
        assert_eq!(one(r#"{"code":"a","host":"h","users":[{"name":"r"},{"name":"r"}]}"#), ImportUser { server: "a".into(), user: "r".into() });
        assert_eq!(one(r#"{"code":"a","host":"h","users":[{"name":"r","key":"~/.ssh/id"}]}"#), ImportKey { server: "a".into(), key: "~/.ssh/id".into() });
        assert_eq!(one(r#"{"code":"a","host":"h"},{"code":"a","host":"g"}"#), ImportDup { server: "a".into() });
        assert_eq!(parse(&" ".repeat(MAX_IMPORT + 1)).unwrap_err(), ImportBig);
    }

    #[test]
    fn duplicates_across_groups() {
        let r = parse(r#"{"version":1,"groups":[{"name":"A","servers":[{"code":"x","host":"h"}]},{"name":"B","servers":[{"code":"x","host":"h"}]}]}"#);
        assert_eq!(r.unwrap_err(), ImportError::ImportDup { server: "x".into() });
    }
}
