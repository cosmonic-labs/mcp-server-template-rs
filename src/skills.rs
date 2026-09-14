//! Skills over MCP — the `io.modelcontextprotocol/skills` extension
//! (MCP 2026-07-28, <https://github.com/modelcontextprotocol/ext-skills>).
//!
//! Every server built from this template ships at least one **skill**: a
//! natural-language playbook that tells a connected agent *when* and *how* to
//! use the server's tools. Tools are the hands; the skill is the manual.
//!
//! The extension is a transport binding for Agent Skills over the existing
//! MCP **resources** primitive plus three methods. This module holds the data;
//! [`crate::server`] is the dispatch:
//!
//! | Wire surface | Here |
//! |---|---|
//! | `capabilities.extensions["io.modelcontextprotocol/skills"]` | [`capability`] |
//! | `skills/list` → `Skill[]` (verbatim frontmatter + digest manifest) | [`entries`] |
//! | `skills/get` → one `Skill` by its `SKILL.md` URI | [`entry`] |
//! | `resources/read` of `skill://<name>/<path>` | [`read`] |
//! | `resources/directory/read` (`directoryRead: true`) | [`directory`] |
//! | `resources/list` — one `SKILL.md` resource per skill | [`resources`] |
//! | `resources/templates/list` | [`resource_templates`] |
//!
//! A `Skill` entry carries the skill's frontmatter **verbatim** and a complete
//! manifest of every file with its SHA-256 digest and byte size. A host builds
//! its registry from entries alone, binds user approval to the manifest, and
//! verifies every byte it later reads against it. Entries are computed once
//! from the embedded bytes, so the manifest cannot disagree with what
//! `resources/read` serves.
//!
//! Discovery is progressive:
//!
//! 1. A client reads the listing (`skills/list`, or for a client without the
//!    extension the [`INDEX_URI`] resource, which mirrors it) once at session
//!    start: skill names plus the trigger descriptions it matches user
//!    requests against.
//! 2. Only when a request matches does it read `skill://<name>/SKILL.md` —
//!    the full playbook.
//! 3. Relative paths inside a `SKILL.md` resolve against the skill root, so
//!    `references/TOOLS.md` is `skill://<name>/references/TOOLS.md`, pulled in
//!    only if the model needs that depth.
//!
//! Skill files are embedded with [`include_str!`], so the component is
//! self-contained: no filesystem, no network, and the playbook version can
//! never drift from the tool implementation it documents. Every lookup
//! matches a URI verbatim against the static [`SKILLS`] table — there is no
//! path to traverse.
//!
//! ## Adding a skill
//!
//! Put it under `skills/<dir>/SKILL.md` (plus any supporting files) and add an
//! entry to [`SKILLS`]. The entry's frontmatter comes from the `SKILL.md` YAML
//! block, so there is exactly one place to edit it — and the extension
//! requires the last segment of the skill's URI path to equal its
//! frontmatter `name`.
//!
//! Directory names are fixed at build time (`include_str!`), but the skill's
//! *name* — the authority in its URIs — is [`env!("CARGO_PKG_NAME")`] for this
//! server's own skill, so renaming the package renames the skill with it.

use std::sync::OnceLock;

use rmcp::model::{JsonObject, MetaObject, Resource, ResourceTemplate};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

/// Identifier of the MCP skills extension this module implements.
pub const EXTENSION_ID: &str = "io.modelcontextprotocol/skills";

/// The `skills/list` method, defined by the extension.
pub const LIST_METHOD: &str = "skills/list";

/// The `skills/get` method, defined by the extension.
pub const GET_METHOD: &str = "skills/get";

/// The optional `resources/directory/read` method, gated on `directoryRead`.
pub const DIRECTORY_READ_METHOD: &str = "resources/directory/read";

/// `mimeType` of a directory resource, per the extension.
pub const DIRECTORY_MIME: &str = "inode/directory";

/// The `_meta` key prefix reserved for skill resources by the extension.
const META_PREFIX: &str = "io.modelcontextprotocol.skills/";

/// URI of the legacy skill catalog — a mirror of `skills/list` for clients
/// that have not adopted the extension.
pub const INDEX_URI: &str = "skill://index.json";

/// Scheme prefix for every skill resource URI.
const SCHEME: &str = "skill://";

/// The entrypoint filename of a skill bundle, per the Agent Skills spec.
const ENTRY: &str = "SKILL.md";

/// A supporting file inside a skill bundle, served at
/// `skill://<skill>/<path>`.
pub struct SkillFile {
    /// Path relative to the skill root, e.g. `references/TOOLS.md`. Matched
    /// verbatim against the requested URI — there is no filesystem lookup and
    /// therefore no traversal to defend against.
    pub path: &'static str,
    /// File contents, embedded at compile time.
    pub text: &'static str,
    /// MIME type reported to the client.
    pub mime_type: &'static str,
}

/// One skill: a `SKILL.md` playbook and its supporting files.
pub struct Skill {
    /// Skill name; the authority segment of its URIs, and — the extension
    /// requires — the `name` in its frontmatter. The server's own skill takes
    /// the package name, so the skill's identity always equals the server's
    /// and neither can drift from the other on a rename.
    pub name: &'static str,
    /// The playbook itself, served at `skill://<name>/SKILL.md`. Held as its
    /// own field rather than as one of `files` so a skill structurally cannot
    /// exist without its entrypoint.
    pub skill_md: &'static str,
    /// Supporting files, if any.
    pub files: &'static [SkillFile],
}

/// Skills this server serves.
///
/// `skills/server/` is this server's own manual — rewrite its contents for
/// your server; nothing here needs renaming, since the skill takes the
/// package name. The `building-mcp-servers` skill that also lives under
/// `skills/` is guidance for *authoring* a server and is deliberately **not**
/// listed here — it is for the developer's coding agent, not for this
/// server's clients.
pub static SKILLS: &[Skill] = &[Skill {
    name: env!("CARGO_PKG_NAME"),
    skill_md: include_str!("../skills/server/SKILL.md"),
    files: &[SkillFile {
        path: "references/TOOLS.md",
        text: include_str!("../skills/server/references/TOOLS.md"),
        mime_type: "text/markdown",
    }],
}];

impl Skill {
    /// URI of this skill's `SKILL.md` — the skill's identity on this server.
    pub fn entry_uri(&self) -> String {
        format!("{SCHEME}{}/{ENTRY}", self.name)
    }

    /// URI of the skill's root directory: the `SKILL.md` URI with `/SKILL.md`
    /// removed and no trailing slash, per the extension.
    pub fn root_uri(&self) -> String {
        format!("{SCHEME}{}", self.name)
    }

    /// URI of a supporting file.
    pub fn file_uri(&self, file: &SkillFile) -> String {
        format!("{SCHEME}{}/{}", self.name, file.path)
    }

    /// The `SKILL.md` YAML frontmatter, verbatim, as a JSON object. Parsed
    /// once per instance.
    pub fn frontmatter(&self) -> &'static Map<String, Value> {
        &self.entry_parts().0
    }

    /// The `description` from the frontmatter — the trigger text a client
    /// matches user requests against.
    pub fn description(&self) -> &'static str {
        self.frontmatter()
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
    }

    /// The extension's `Skill` entry: `{uri, frontmatter, resources}`, with a
    /// complete manifest — `SKILL.md` first, then every supporting file, each
    /// with the SHA-256 digest and byte size of exactly what [`read`] serves.
    pub fn entry(&self) -> &'static Value {
        &self.entry_parts().1
    }

    fn entry_parts(&self) -> &'static (Map<String, Value>, Value) {
        static ENTRIES: OnceLock<Vec<(Map<String, Value>, Value)>> = OnceLock::new();
        let entries = ENTRIES.get_or_init(|| SKILLS.iter().map(Skill::build_entry).collect());
        let index = SKILLS
            .iter()
            .position(|s| std::ptr::eq(s, self))
            .expect("Skill::entry called on a skill outside SKILLS");
        &entries[index]
    }

    fn build_entry(&self) -> (Map<String, Value>, Value) {
        let frontmatter = parse_frontmatter(self.skill_md).unwrap_or_default();
        let mut resources = Vec::with_capacity(self.files.len() + 1);
        resources.push(json!({
            "uri": self.entry_uri(),
            "digest": digest(self.skill_md),
            "size": self.skill_md.len(),
        }));
        for file in self.files {
            resources.push(json!({
                "uri": self.file_uri(file),
                "digest": digest(file.text),
                "size": file.text.len(),
            }));
        }
        let entry = json!({
            "uri": self.entry_uri(),
            "frontmatter": frontmatter,
            "resources": resources,
        });
        (frontmatter, entry)
    }
}

/// The extension's per-server settings, declared under
/// `capabilities.extensions[EXTENSION_ID]`. `directoryRead: true` promises
/// `resources/directory/read` for every directory in the skill namespace,
/// which [`directory`] keeps.
pub fn capability() -> JsonObject {
    let mut settings = JsonObject::new();
    settings.insert("directoryRead".into(), Value::Bool(true));
    settings
}

/// Every skill's entry, in table order — the `skills` array of `skills/list`.
/// One page: a server embedding enough skills for that to be unwieldy should
/// honour the request's `cursor` and set `nextCursor` — never splitting one
/// entry across pages.
pub fn entries() -> Vec<Value> {
    SKILLS.iter().map(|s| s.entry().clone()).collect()
}

/// The entry for the skill whose `SKILL.md` URI this is — `skills/get`.
/// `None` when this server serves no skill there (the caller answers
/// `-32602`). Only the exact `SKILL.md` URI names a skill.
pub fn entry(uri: &str) -> Option<&'static Value> {
    let (name, path) = uri.strip_prefix(SCHEME)?.split_once('/')?;
    if path != ENTRY {
        return None;
    }
    SKILLS.iter().find(|s| s.name == name).map(Skill::entry)
}

/// The legacy catalog served at [`INDEX_URI`]: the same entries `skills/list`
/// returns, wrapped for a client that reads it as a resource. Built on every
/// read rather than stored, so it cannot fall out of step with the files.
pub fn index_json() -> String {
    let index = json!({
        "schemaVersion": "2",
        "extension": EXTENSION_ID,
        "server": {
            "name": env!("CARGO_PKG_NAME"),
            "version": env!("CARGO_PKG_VERSION"),
        },
        "usage": "A mirror of this server's `skills/list` for clients without the \
                  io.modelcontextprotocol/skills extension. Each entry is one skill: its \
                  SKILL.md `uri`, its `frontmatter` (the `description` is what a task is \
                  matched against), and `resources` — every file of the skill with its \
                  SHA-256 digest and size. A relative path a playbook names resolves to \
                  skill://<skill>/<that path>, and every such URI is in `resources`.",
        "skills": entries(),
    });
    // The value is built from `&'static str` and owned values — serialization
    // cannot fail, but never panic in a component: a trap kills the instance.
    serde_json::to_string_pretty(&index).unwrap_or_else(|_| String::from(r#"{"skills":[]}"#))
}

/// The skill resources for `resources/list`: the legacy catalog and one
/// `SKILL.md` per skill, with the metadata the extension prescribes — `name`
/// and `description` from the frontmatter, `text/markdown`, and the remaining
/// frontmatter fields under `_meta` with the reserved prefix.
///
/// Supporting files are deliberately not listed: the manifest enumerates
/// them with digests, `resources/directory/read` walks them, and they are
/// readable by URI.
pub fn resources() -> Vec<Resource> {
    let mut resources = vec![Resource::new(INDEX_URI, "skill-index")
        .with_title("Skill catalog")
        .with_description(
            "The skills this server publishes — the same entries as `skills/list`, for a \
             client without the io.modelcontextprotocol/skills extension: each skill's \
             SKILL.md URI, its frontmatter (name and trigger description), and its file \
             manifest with SHA-256 digests.",
        )
        .with_mime_type("application/json")];

    for skill in SKILLS {
        let mut meta = JsonObject::new();
        for (key, value) in skill.frontmatter() {
            if key != "name" && key != "description" {
                meta.insert(format!("{META_PREFIX}{key}"), value.clone());
            }
        }
        let mut resource = Resource::new(skill.entry_uri(), skill.name)
            .with_title(format!("{} skill", skill.name))
            .with_description(skill.description())
            .with_mime_type("text/markdown")
            .with_size(skill.skill_md.len() as u64);
        if !meta.is_empty() {
            resource = resource.with_meta(MetaObject::from(meta));
        }
        resources.push(resource);
    }
    resources
}

/// RFC 6570 templates for `resources/templates/list`, so a client can build
/// skill URIs without enumerating them.
pub fn resource_templates() -> Vec<ResourceTemplate> {
    vec![
        ResourceTemplate::new("skill://{skill}/SKILL.md", "skill-playbook")
            .with_title("Skill playbook")
            .with_description(
                "The SKILL.md of a named skill. Skill names come from skills/list (or the \
                 skill://index.json catalog).",
            )
            .with_mime_type("text/markdown"),
        ResourceTemplate::new("skill://{skill}/{+path}", "skill-file")
            .with_title("Skill supporting file")
            .with_description(
                "A file bundled with a skill, at the path its SKILL.md names relative to the \
                 skill root. Every such URI is in the skill's manifest.",
            ),
    ]
}

/// Resolves a `skill://` URI to `(mime_type, contents)`, or `None` if this
/// server serves nothing at that URI.
pub fn read(uri: &str) -> Option<(&'static str, String)> {
    if uri == INDEX_URI {
        return Some(("application/json", index_json()));
    }

    let (name, path) = uri.strip_prefix(SCHEME)?.split_once('/')?;
    let skill = SKILLS.iter().find(|skill| skill.name == name)?;
    if path == ENTRY {
        return Some(("text/markdown", skill.skill_md.to_owned()));
    }
    let file = skill.files.iter().find(|file| file.path == path)?;
    Some((file.mime_type, file.text.to_owned()))
}

/// The direct children of a directory resource — `resources/directory/read`.
///
/// `None` when the URI is not a directory this server serves (a file, an
/// unknown skill, a trailing slash, a traversal), which the caller answers
/// with `-32602`. Directories are implied by the file paths: `skill://<name>`
/// is every skill's root, and `skill://<name>/references` exists because a
/// file lives under it. A subdirectory is listed once, as `inode/directory`.
pub fn directory(uri: &str) -> Option<Vec<Resource>> {
    let rest = uri.strip_prefix(SCHEME)?;
    let (name, dir) = match rest.split_once('/') {
        Some((name, dir)) => (name, Some(dir)),
        None => (rest, None),
    };
    if name.is_empty() {
        return None;
    }
    let skill = SKILLS.iter().find(|skill| skill.name == name)?;
    let dir = match dir {
        None => "",
        Some("") => return None, // `skill://<name>/` — a trailing slash is not the root
        Some(dir) => dir,
    };
    if !dir.is_empty()
        && dir
            .split('/')
            .any(|seg| seg.is_empty() || seg == "." || seg == "..")
    {
        return None;
    }

    let files = std::iter::once((ENTRY, "text/markdown", skill.skill_md.len())).chain(
        skill
            .files
            .iter()
            .map(|f| (f.path, f.mime_type, f.text.len())),
    );
    let mut out: Vec<Resource> = Vec::new();
    let mut subdirs: Vec<&str> = Vec::new();
    let mut is_dir = dir.is_empty();
    for (path, mime, len) in files {
        let below = if dir.is_empty() {
            path
        } else {
            let Some(below) = path.strip_prefix(dir).and_then(|b| b.strip_prefix('/')) else {
                continue;
            };
            below
        };
        is_dir = true;
        match below.split_once('/') {
            None => out.push(
                Resource::new(format!("{SCHEME}{name}/{path}"), below)
                    .with_mime_type(mime)
                    .with_size(len as u64),
            ),
            Some((child, _)) => {
                if !subdirs.contains(&child) {
                    subdirs.push(child);
                    let child_path = if dir.is_empty() {
                        child.to_string()
                    } else {
                        format!("{dir}/{child}")
                    };
                    out.push(
                        Resource::new(format!("{SCHEME}{name}/{child_path}"), child)
                            .with_mime_type(DIRECTORY_MIME),
                    );
                }
            }
        }
    }
    is_dir.then_some(out)
}

/// `sha256:<64 lowercase hex>` of the raw bytes, the extension's digest form.
fn digest(text: &str) -> String {
    let mut out = String::with_capacity("sha256:".len() + 64);
    out.push_str("sha256:");
    for byte in Sha256::digest(text.as_bytes()) {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// The YAML frontmatter of a Markdown file as a JSON object, verbatim.
///
/// The extension requires the entry's `frontmatter` to be identical, field by
/// field, to what a host parses out of the served `SKILL.md` — so this is a
/// real YAML parse. `None` when there is no frontmatter block or it is not a
/// mapping. CRLF-tolerant, for a Windows checkout.
fn parse_frontmatter(markdown: &str) -> Option<Map<String, Value>> {
    let body = markdown
        .strip_prefix("---\n")
        .or_else(|| markdown.strip_prefix("---\r\n"))?;
    let end = body.find("\n---")?;
    let yaml: serde_yaml::Value = serde_yaml::from_str(&body[..end]).ok()?;
    match serde_json::to_value(yaml).ok()? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_skill_is_named_after_the_package_and_has_a_trigger() {
        let skill = &SKILLS[0];
        assert_eq!(skill.name, env!("CARGO_PKG_NAME"));
        assert_eq!(skill.frontmatter()["name"], skill.name);
        assert!(skill.description().len() > 40, "description is the trigger");
    }

    #[test]
    fn manifest_digests_and_sizes_match_what_read_serves() {
        for skill in SKILLS {
            let manifest = skill.entry()["resources"].as_array().unwrap();
            assert_eq!(manifest.len(), skill.files.len() + 1);
            assert_eq!(manifest[0]["uri"], skill.entry_uri());
            for r in manifest {
                let uri = r["uri"].as_str().unwrap();
                let (_, body) = read(uri).expect("manifest lists an unreadable file");
                assert_eq!(r["size"].as_u64().unwrap(), body.len() as u64);
                assert_eq!(r["digest"].as_str().unwrap(), digest(&body));
            }
        }
    }

    #[test]
    fn get_resolves_only_the_exact_skill_md_uri() {
        let skill = &SKILLS[0];
        assert_eq!(entry(&skill.entry_uri()), Some(skill.entry()));
        assert!(entry(&skill.root_uri()).is_none());
        assert!(entry(&skill.file_uri(&skill.files[0])).is_none());
        assert!(entry("skill://nope/SKILL.md").is_none());
        assert!(entry(INDEX_URI).is_none());
    }

    #[test]
    fn the_index_mirrors_skills_list() {
        let index: Value = serde_json::from_str(&index_json()).unwrap();
        assert_eq!(index["extension"], EXTENSION_ID);
        assert_eq!(index["skills"], Value::Array(entries()));
    }

    #[test]
    fn resources_list_is_the_catalog_plus_one_playbook_per_skill() {
        let listed = resources();
        assert_eq!(listed.len(), SKILLS.len() + 1);
        for r in &listed {
            let (mime, body) = read(&r.uri).expect("listed but unreadable");
            assert!(!body.is_empty());
            assert_eq!(r.mime_type.as_deref(), Some(mime));
        }
    }

    #[test]
    fn directory_read_walks_the_tree_and_refuses_the_rest() {
        let skill = &SKILLS[0];
        let root = directory(&skill.root_uri()).expect("root");
        let names: Vec<_> = root.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["SKILL.md", "references"]);
        let refs = directory(&format!("{}/references", skill.root_uri())).expect("references");
        assert_eq!(refs.len(), skill.files.len());
        for uri in [
            skill.entry_uri(),
            format!("{}/", skill.root_uri()),
            format!("{}/nope", skill.root_uri()),
            format!("{}/../x", skill.root_uri()),
            "skill://nope".into(),
            INDEX_URI.into(),
        ] {
            assert!(directory(&uri).is_none(), "{uri} read as a directory");
        }
    }

    #[test]
    fn read_resolves_nothing_outside_the_static_table() {
        let name = SKILLS[0].name;
        for uri in [
            format!("skill://{name}/../../../etc/passwd"),
            format!("skill://{name}/./SKILL.md"),
            format!("skill://{name}/SKILL.md/"),
            "skill://../SKILL.md".into(),
            "file:///etc/passwd".into(),
            String::new(),
        ] {
            assert!(read(&uri).is_none(), "resolved something for {uri:?}");
        }
    }

    #[test]
    fn frontmatter_is_verbatim_yaml() {
        const DOC: &str = "---\r\nname: demo\r\ndescription: \"quoted\"\r\nmetadata:\r\n  version: \"1\"\r\n---\r\nbody\r\n";
        let fm = parse_frontmatter(DOC).unwrap();
        assert_eq!(fm["name"], "demo");
        assert_eq!(fm["description"], "quoted");
        assert_eq!(fm["metadata"]["version"], "1");
        assert!(parse_frontmatter("no frontmatter\n").is_none());
    }
}
