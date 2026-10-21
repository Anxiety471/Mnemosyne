//! Memory vault: an Obsidian-style set of Markdown notes kept in the `/memory` folder.
//!
//! Notes are ordinary Drawer files, so they are versioned, searchable, shareable and
//! editable in the web UI like any other file. On top of that this module understands the
//! Obsidian conventions: `[[wikilinks]]` (with `|alias` and `#heading` parts), `#tags` in the
//! text, and `tags:` in YAML frontmatter. The link graph and tag index are derived from the
//! files on every request, so there is no second copy of the truth to keep in step.
use std::collections::BTreeMap;

use axum::{
    Json,
    extract::{Query, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    error::{Error, Result},
    service::{Drawer, Ref, WriteOpts},
};

/// Folder holding the vault.
pub const VAULT: &str = "/memory";
const MAX_NOTE_BYTES: usize = 1024 * 1024;
const MAX_NOTES: usize = 5000;

/// What the indexer found in one note.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Parsed {
    pub tags: Vec<String>,
    pub links: Vec<String>,
}

fn split_frontmatter(text: &str) -> (Option<&str>, &str) {
    let Some(rest) = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    else {
        return (None, text);
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return (Some(&rest[..offset]), &rest[offset + line.len()..]);
        }
        offset += line.len();
    }
    (None, text)
}

fn clean_tag(t: &str) -> Option<String> {
    let t = t
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .trim_start_matches('#')
        .to_lowercase();
    let ok = !t.is_empty()
        && t.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '/'))
        && t.chars().any(|c| !c.is_ascii_digit());
    ok.then_some(t)
}

fn frontmatter_tags(fm: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_list = false;
    for line in fm.lines() {
        if let Some((key, value)) = line.split_once(':')
            && matches!(key.trim(), "tags" | "tag")
        {
            let value = value.trim().trim_start_matches('[').trim_end_matches(']');
            out.extend(
                value
                    .split(|c: char| c == ',' || c.is_whitespace())
                    .filter_map(clean_tag),
            );
            in_list = value.is_empty();
        } else if in_list && let Some(item) = line.trim_start().strip_prefix('-') {
            out.extend(clean_tag(item));
        } else if !line.trim().is_empty() {
            in_list = false;
        }
    }
    out
}

fn push_unique(v: &mut Vec<String>, item: String) {
    if !v.contains(&item) {
        v.push(item);
    }
}

/// Tags and wikilink targets of a note. Fenced code blocks and inline code are ignored.
pub fn parse(text: &str) -> Parsed {
    let (fm, body) = split_frontmatter(text);
    let mut out = Parsed::default();
    for t in fm.map(frontmatter_tags).unwrap_or_default() {
        push_unique(&mut out.tags, t);
    }
    let mut fenced = false;
    for line in body.lines() {
        if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        // Drop inline code spans so `#include` and `[[x]]` inside them don't count.
        let line: String = line
            .split('`')
            .enumerate()
            .filter(|(i, _)| i % 2 == 0)
            .map(|(_, s)| s)
            .collect::<Vec<_>>()
            .join(" ");
        let mut rest = line.as_str();
        while let Some(start) = rest.find("[[") {
            let after = &rest[start + 2..];
            let Some(end) = after.find("]]") else { break };
            let target = after[..end]
                .split('|')
                .next()
                .unwrap_or("")
                .split('#')
                .next()
                .unwrap_or("")
                .trim();
            if !target.is_empty() {
                push_unique(&mut out.links, target.to_string());
            }
            rest = &after[end + 2..];
        }
        let mut prev = ' ';
        let chars: Vec<char> = line.chars().collect();
        for (i, &c) in chars.iter().enumerate() {
            if c == '#' && prev.is_whitespace() {
                let word: String = chars[i + 1..]
                    .iter()
                    .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '/'))
                    .collect();
                if let Some(t) = clean_tag(&word) {
                    push_unique(&mut out.tags, t);
                }
            }
            prev = c;
        }
    }
    out
}

/// Note name of a vault path: the file name without `.md`.
fn stem(path: &str) -> &str {
    let file = path.rsplit('/').next().unwrap_or(path);
    file.strip_suffix(".md").unwrap_or(file)
}

/// Path of a note relative to the vault, without `.md`: `projects/drawer`.
fn rel(path: &str) -> &str {
    let p = path
        .strip_prefix(VAULT)
        .unwrap_or(path)
        .trim_start_matches('/');
    p.strip_suffix(".md").unwrap_or(p)
}

struct Note {
    id: String,
    path: String,
    modified_at: String,
    modified_by: String,
    size: i64,
    revision: i64,
    parsed: Parsed,
}

/// Find the note a link or name refers to. Like Obsidian, a bare name matches any folder and
/// the shortest path wins; `folder/name` matches a path suffix.
fn resolve<'a>(notes: &'a [Note], target: &str) -> Option<&'a Note> {
    let t = target
        .trim()
        .trim_end_matches(".md")
        .trim_start_matches('/')
        .to_lowercase();
    notes
        .iter()
        .filter(|n| {
            let r = rel(&n.path).to_lowercase();
            r == t || r.ends_with(&format!("/{t}"))
        })
        .min_by(|a, b| {
            a.path
                .len()
                .cmp(&b.path.len())
                .then_with(|| a.path.cmp(&b.path))
        })
}

/// List the vault's notes from a metadata walk, using the parse cache so only notes whose
/// head revision changed are read from disk and re-parsed. `text` is left empty; call
/// `note_text` when a caller needs the body.
async fn load(svc: &Drawer) -> Result<Vec<Note>> {
    let nodes = match svc.walk(VAULT, 10).await {
        Ok((nodes, _)) => nodes,
        Err(Error::NotFound(_)) => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut cache = svc.vault_cache.lock().await;
    let mut notes = Vec::new();
    for n in nodes
        .into_iter()
        .filter(|n| n.kind == "file" && n.name.ends_with(".md"))
        .take(MAX_NOTES)
    {
        if n.size.unwrap_or(0) as usize > MAX_NOTE_BYTES {
            continue;
        }
        let head = n.head_revision.unwrap_or(0);
        let parsed = match cache.get(&n.id) {
            Some((rev, parsed)) if *rev == head => parsed.clone(),
            _ => {
                let (rev, data) = svc.read(Ref::Id(&n.id), None).await?;
                let parsed = parse(&String::from_utf8_lossy(&data));
                cache.insert(n.id.clone(), (rev.revision, parsed.clone()));
                parsed
            }
        };
        notes.push(Note {
            parsed,
            id: n.id,
            path: n.path,
            modified_at: n.modified_at,
            modified_by: n.modified_by.unwrap_or_default(),
            size: n.size.unwrap_or(0),
            revision: head,
        });
    }
    // Drop cache entries for notes that no longer exist.
    let live: std::collections::HashSet<&String> = notes.iter().map(|n| &n.id).collect();
    cache.retain(|id, _| live.contains(id));
    Ok(notes)
}

/// Body of a note, read on demand (the parse cache does not hold note text).
async fn note_text(svc: &Drawer, id: &str, revision: i64) -> Result<String> {
    let (_, data) = svc.read(Ref::Id(id), Some(revision)).await?;
    Ok(String::from_utf8_lossy(&data).into_owned())
}

#[derive(Serialize)]
pub struct Summary {
    pub name: String,
    pub path: String,
    pub tags: Vec<String>,
    pub modified_at: String,
    pub modified_by: String,
    pub size: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

fn summary(n: &Note, snippet: Option<String>) -> Summary {
    Summary {
        name: stem(&n.path).to_string(),
        path: n.path.clone(),
        tags: n.parsed.tags.clone(),
        modified_at: n.modified_at.clone(),
        modified_by: n.modified_by.clone(),
        size: n.size,
        snippet,
    }
}

fn tag_matches(tags: &[String], want: &str) -> bool {
    let w = want.trim().trim_start_matches('#').to_lowercase();
    // Like Obsidian, #project also finds #project/drawer.
    tags.iter()
        .any(|t| *t == w || t.starts_with(&format!("{w}/")))
}

/// Notes, newest first, optionally narrowed to a tag and/or a case-insensitive text query
/// matched against the note name and body.
pub async fn list(
    svc: &Drawer,
    tag: Option<&str>,
    query: Option<&str>,
    limit: usize,
) -> Result<Vec<Summary>> {
    let q = query
        .map(|q| q.trim().to_lowercase())
        .filter(|q| !q.is_empty());
    let mut notes = load(svc).await?;
    notes.sort_by(|a, b| {
        b.modified_at
            .cmp(&a.modified_at)
            .then_with(|| a.path.cmp(&b.path))
    });
    let mut out = Vec::new();
    for n in &notes {
        if tag.is_some_and(|t| !tag_matches(&n.parsed.tags, t)) {
            continue;
        }
        let snippet = match &q {
            None => None,
            Some(q) => {
                // Text is only needed for the snippet; read it lazily for the few survivors.
                let text = note_text(svc, &n.id, n.revision).await?;
                if let Some(line) = text.lines().find(|l| l.to_lowercase().contains(q)) {
                    Some(line.trim().chars().take(200).collect())
                } else if stem(&n.path).to_lowercase().contains(q) {
                    Some(String::new())
                } else {
                    continue;
                }
            }
        };
        out.push(summary(n, snippet));
        if out.len() >= limit.clamp(1, 500) {
            break;
        }
    }
    Ok(out)
}

#[derive(Serialize)]
pub struct Link {
    pub target: String,
    /// Path of the note it resolves to; absent for a link to a note that doesn't exist yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Serialize)]
pub struct Detail {
    #[serde(flatten)]
    pub note: Summary,
    pub revision: i64,
    pub content: String,
    pub links: Vec<Link>,
    pub backlinks: Vec<Summary>,
}

pub async fn get(svc: &Drawer, name: &str) -> Result<Detail> {
    let notes = load(svc).await?;
    let n = resolve(&notes, name).ok_or_else(|| Error::NotFound(format!("memory note {name}")))?;
    let links = n
        .parsed
        .links
        .iter()
        .map(|t| Link {
            target: t.clone(),
            path: resolve(&notes, t).map(|r| r.path.clone()),
        })
        .collect();
    let backlinks = notes
        .iter()
        .filter(|o| {
            o.id != n.id
                && o.parsed
                    .links
                    .iter()
                    .any(|t| resolve(&notes, t).is_some_and(|r| r.id == n.id))
        })
        .map(|o| summary(o, None))
        .collect();
    let content = note_text(svc, &n.id, n.revision).await?;
    Ok(Detail {
        note: summary(n, None),
        revision: n.revision,
        content,
        links,
        backlinks,
    })
}

#[derive(Serialize)]
pub struct GraphNode {
    pub id: String,
    pub name: String,
    /// Absent for a note that is linked to but doesn't exist yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub tags: Vec<String>,
    /// Links out of this note and links into it.
    pub out: usize,
    pub inbound: usize,
}

#[derive(Serialize)]
pub struct Graph {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<(String, String)>,
}

/// The whole link graph: one node per note, plus one for each link target that has no note
/// yet (like Obsidian's unresolved links), and one edge per linked pair.
pub async fn graph(svc: &Drawer) -> Result<Graph> {
    let notes = load(svc).await?;
    let mut nodes: Vec<GraphNode> = notes
        .iter()
        .map(|n| GraphNode {
            id: n.path.clone(),
            name: stem(&n.path).to_string(),
            path: Some(n.path.clone()),
            tags: n.parsed.tags.clone(),
            out: 0,
            inbound: 0,
        })
        .collect();
    let mut index: BTreeMap<String, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.clone(), i))
        .collect();
    let mut edges: Vec<(String, String)> = Vec::new();
    for n in &notes {
        for t in &n.parsed.links {
            let target = match resolve(&notes, t) {
                Some(r) => r.path.clone(),
                None => {
                    let id = format!("?{}", t.to_lowercase());
                    index.entry(id.clone()).or_insert_with(|| {
                        nodes.push(GraphNode {
                            id: id.clone(),
                            name: t.clone(),
                            path: None,
                            tags: Vec::new(),
                            out: 0,
                            inbound: 0,
                        });
                        nodes.len() - 1
                    });
                    id
                }
            };
            let edge = (n.path.clone(), target);
            if edge.0 != edge.1 && !edges.contains(&edge) {
                nodes[index[&edge.0]].out += 1;
                nodes[index[&edge.1]].inbound += 1;
                edges.push(edge);
            }
        }
    }
    Ok(Graph { nodes, edges })
}

/// Tag counts, most used first.
pub async fn tags(svc: &Drawer) -> Result<Vec<(String, usize)>> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for n in load(svc).await? {
        for t in n.parsed.tags {
            *counts.entry(t).or_default() += 1;
        }
    }
    let mut v: Vec<_> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Ok(v)
}

fn note_path(name: &str) -> Result<String> {
    let name = name.trim().trim_matches('/');
    let name = name.strip_suffix(".md").unwrap_or(name);
    let bad = name.is_empty()
        || name.len() > 200
        || name
            .split('/')
            .any(|s| s.trim().is_empty() || s == "." || s == "..")
        || name
            .chars()
            .any(|c| c.is_control() || "\\:*?\"<>|#[]^".contains(c));
    if bad {
        return Err(Error::Invalid(format!(
            "invalid note name {name:?}: use letters, digits, spaces, - _ and / for folders"
        )));
    }
    Ok(format!("{VAULT}/{name}.md"))
}

/// Create or replace a note, or with `append` add to the end of it. An existing note of the
/// same name (found the Obsidian way) is updated in place. `tags` become frontmatter on a new
/// note, trailing `#tags` when appending, and a frontmatter replacement on an existing note.
pub async fn save(
    svc: &Drawer,
    name: &str,
    content: &str,
    tags: &[String],
    append: bool,
    actor: &str,
) -> Result<Value> {
    let notes = load(svc).await?;
    let path = match resolve(&notes, name) {
        Some(n) => n.path.clone(),
        None => note_path(name)?,
    };
    let tags: Vec<String> = tags
        .iter()
        .map(|t| clean_tag(t).ok_or_else(|| Error::Invalid(format!("invalid tag {t:?}"))))
        .collect::<Result<_>>()?;
    let existing = notes
        .iter()
        .find(|n| n.path == path)
        .map(|n| (n.id.clone(), n.revision));
    let fetched = match &existing {
        Some((id, rev)) => note_text(svc, id, *rev).await?,
        None => String::new(),
    };
    let text = if append && existing.is_some() {
        let mut t = fetched.trim_end().to_string();
        t.push_str("\n\n");
        t.push_str(content.trim());
        if !tags.is_empty() {
            t.push('\n');
            t.push_str(
                &tags
                    .iter()
                    .map(|t| format!("#{t}"))
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        t.push('\n');
        t
    } else {
        let mut t = String::new();
        let body = content.trim_end().to_string();
        if !tags.is_empty() {
            // Apply the explicit tags to the resulting frontmatter: replace an existing
            // `tags:`/`tag:` block (old tags are dropped), or add one when there is none.
            if split_frontmatter(content).0.is_some() {
                // Content carries its own frontmatter: replace its tags in place.
                t.push_str(&set_frontmatter_tags(content, &tags));
            } else {
                t.push_str(&format!("---\ntags: [{}]\n---\n\n", tags.join(", ")));
                t.push_str(&body);
            }
        } else {
            t.push_str(&body);
        }
        t.push('\n');
        t
    };
    if text.len() > MAX_NOTE_BYTES {
        return Err(Error::Invalid("note is larger than 1 MiB".into()));
    }
    let mut opts = WriteOpts::new(
        actor,
        if append {
            "append memory"
        } else {
            "save memory"
        },
    );
    opts.if_head_revision = existing.as_ref().map(|(_, rev)| *rev);
    if existing.is_none() {
        opts.if_head_revision = Some(0);
    }
    let out = svc.write(&path, text.as_bytes(), opts).await?;
    let parsed = parse(&text);
    Ok(
        json!({ "path": path, "name": stem(&path), "revision": out.revision, "tags": parsed.tags, "links": parsed.links, "created": existing.is_none() }),
    )
}

/// Replace the frontmatter `tags:`/`tag:` entry of `text` with `tags`. Body `#tags` are left
/// untouched. Returns the full text (frontmatter plus body).
fn set_frontmatter_tags(text: &str, tags: &[String]) -> String {
    let (fm, body) = split_frontmatter(text);
    let line = format!("tags: [{}]", tags.join(", "));
    match fm {
        None => format!("---\n{line}\n---\n\n{body}"),
        Some(fm) => {
            let mut out = String::new();
            let mut replaced = false;
            for l in fm.lines() {
                if let Some((key, _)) = l.split_once(':')
                    && matches!(key.trim(), "tags" | "tag")
                    && !replaced
                {
                    out.push_str(&line);
                    out.push('\n');
                    replaced = true;
                } else {
                    out.push_str(l);
                    out.push('\n');
                }
            }
            format!("{out}{body}")
        }
    }
}

/// Move a note to the trash. Drawer's trash is reversible, so the note and its history can be
/// restored; links to it become unresolved until it is written again.
pub async fn forget(svc: &Drawer, name: &str, actor: &str) -> Result<Value> {
    let notes = load(svc).await?;
    let n = resolve(&notes, name).ok_or_else(|| Error::NotFound(format!("memory note {name}")))?;
    let out = svc.trash(Ref::Id(&n.id), None, actor).await?;
    Ok(json!({ "path": n.path, "name": stem(&n.path), "trashed": out.trashed, "id": n.id }))
}

// ----- REST ----------------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ListQ {
    tag: Option<String>,
    q: Option<String>,
    limit: Option<usize>,
}

pub async fn list_notes(State(s): State<Drawer>, Query(q): Query<ListQ>) -> Result<Json<Value>> {
    Ok(Json(
        json!({ "notes": list(&s, q.tag.as_deref(), q.q.as_deref(), q.limit.unwrap_or(50)).await? }),
    ))
}

#[derive(Deserialize)]
pub struct NameQ {
    name: String,
}

pub async fn read_note(State(s): State<Drawer>, Query(q): Query<NameQ>) -> Result<Json<Detail>> {
    Ok(Json(get(&s, &q.name).await?))
}

#[derive(Deserialize)]
pub struct SaveBody {
    name: String,
    content: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    append: bool,
}

pub async fn delete_note(
    State(s): State<Drawer>,
    headers: HeaderMap,
    Query(q): Query<NameQ>,
) -> Result<Json<Value>> {
    let actor = headers
        .get("x-drawer-author")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .unwrap_or("api");
    Ok(Json(forget(&s, &q.name, actor).await?))
}

pub async fn save_note(
    State(s): State<Drawer>,
    headers: HeaderMap,
    Json(b): Json<SaveBody>,
) -> Result<Json<Value>> {
    let actor = headers
        .get("x-drawer-author")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .unwrap_or("api");
    Ok(Json(
        save(&s, &b.name, &b.content, &b.tags, b.append, actor).await?,
    ))
}

pub async fn read_graph(State(s): State<Drawer>) -> Result<Json<Graph>> {
    Ok(Json(graph(&s).await?))
}

pub async fn list_tags(State(s): State<Drawer>) -> Result<Json<Value>> {
    let tags: Vec<Value> = tags(&s)
        .await?
        .into_iter()
        .map(|(tag, count)| json!({ "tag": tag, "count": count }))
        .collect();
    Ok(Json(json!({ "tags": tags })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tags_and_links() {
        let p = parse(
            "---\ntags: [Work, \"ai\"]\n---\n# Title\nSee [[Other Note|alias]] and [[dir/x#Head]] #idea #2024 `#no` `[[no]]`\n```\n#code [[nope]]\n```\n",
        );
        assert_eq!(p.tags, ["work", "ai", "idea"]);
        assert_eq!(p.links, ["Other Note", "dir/x"]);
    }

    #[test]
    fn frontmatter_list_form() {
        assert_eq!(
            parse("---\ntags:\n  - a\n  - b/c\ntitle: x\n---\nbody").tags,
            ["a", "b/c"]
        );
    }

    #[test]
    fn names() {
        assert_eq!(
            note_path("projects/Drawer").unwrap(),
            "/memory/projects/Drawer.md"
        );
        assert!(note_path("../x").is_err());
        assert!(note_path("a#b").is_err());
    }
}
