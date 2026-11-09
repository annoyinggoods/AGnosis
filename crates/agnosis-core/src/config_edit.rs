//! Surgical edits to the user's `config.toml`.
//!
//! WHY this module exists (audit 2026-10-20 §3.3 and §3.9): the config
//! file belongs to the **user**, and both the CLI and the editor used to
//! rewrite it wholesale through the [`crate::config::Config`] struct. That
//! had three consequences, all of them user-visible:
//!
//! 1. **Assignments could be deleted by an unrelated edit.** The editor's
//!    ASSIGN panel loaded the config with the *sanitizing* loader, which
//!    drops every assignment whose drawn asset does not exist (yet), and
//!    then wrote the whole file back — so one click could delete the
//!    user's other pending assignments.
//! 2. **Comments and unknown keys were lost.** A `Config` round trip
//!    serializes only the keys the struct knows, so a user's own key — or
//!    a comment explaining one — did not survive.
//! 3. **A crash mid-write truncated the file**, and there was no backup
//!    and no recovery command, so a corrupt config bricked every
//!    config-reading command.
//!
//! The fix is to stop rewriting the file wholesale. Only the
//! `[assignments]` table is replaced, and it is replaced **in place** in
//! the original text: every comment, every unknown key and every other
//! table survives byte-for-byte. The write is atomic (temp file + rename)
//! and keeps a `.bak` next to the config.
//!
//! The surgical path is verified before it is written: the spliced
//! document must parse **and** must carry exactly the table that was
//! meant to be written. A document in an unexpected shape (an inline
//! `assignments = { … }` key, say) falls back to the whole-file rewrite,
//! so the edit always lands in a valid file.

use std::ops::Range;
use std::path::Path;

use crate::config::{AssignmentsConfig, Config, ConfigError, Repair};

/// The suffix of the backup file the config write keeps.
const BACKUP_SUFFIX: &str = ".bak";

/// Write ONLY the `[assignments]` table into the config at `path`.
///
/// The path is a parameter, so the tests write into a temp dir and never
/// touch the user's real config file.
///
/// The user's file is edited in place: the `[assignments]` section is
/// replaced and everything else — comments, unknown keys, the other
/// tables — is left exactly as it was.
pub fn write_assignments(path: &Path, table: &AssignmentsConfig) -> Result<(), ConfigError> {
    // The read decides whether the file is fresh — an `exists()` check
    // first is a race: the file can appear between the two, and the
    // fresh path would then overwrite it with the default config
    // (audit 2026-10-23 §F-058).
    let original = match read_to_string(path) {
        Ok(text) => text,
        Err(e) if is_not_found(&e) => return write_fresh_config(path, table),
        Err(e) => return Err(e),
    };
    match splice_assignments(&original, table) {
        Some(spliced) if assignments_survive(&spliced, table) => write_atomic(path, &spliced),
        _ => write_whole_config(path, table),
    }
}

/// Is this error "the file is not there"?
fn is_not_found(err: &ConfigError) -> bool {
    matches!(
        err,
        ConfigError::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound
    )
}

/// Read the config, or the serialized default when the file is not there
/// yet.
///
/// WHY: `write_assignments` creates a fresh file from the whole default
/// config when nothing is there (there is nothing of the user's to
/// preserve), but `write_key` and `write_table` used to fail with an io
/// error instead — so the settings panel could not store a value on a
/// machine whose config file had not been written yet
/// (audit 2026-10-23 §F-064). The read decides, so there is no
/// `exists()` race here either (audit 2026-10-23 §F-058).
///
/// `pub(crate)`: the multi-table composers (`preset::apply_preset_to_file`,
/// `package::apply_look_from_fragment`) build every splice **in memory**
/// and land them in one atomic write (audit 2026-10-23 §F-051), so they
/// need the same read-or-default starting text the single-table writers
/// have.
pub(crate) fn read_or_default(path: &Path) -> Result<String, ConfigError> {
    match read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if is_not_found(&e) => Ok(toml::to_string_pretty(&Config::default())?),
        Err(e) => Err(e),
    }
}

/// Write one array-of-tables section into the config, **in place**.
///
/// The section is `[[name]]` — one table per entry. The whole block is
/// replaced in place, so the user's comments and the other tables
/// survive.
///
/// A file that does not exist yet is created from the whole default
/// config, the same fresh path `write_key` and `write_table` have
/// (audit 2026-10-23 §F-064) — the PERSONALITY actions write through
/// this function, and a machine whose config has not been written yet
/// must not make them fail.
pub fn write_array_table(
    path: &Path,
    name: &str,
    entries: &[Vec<(String, Option<String>)>],
) -> Result<(), ConfigError> {
    let original = read_or_default(path)?;
    let section = render_array_table(name, entries, newline_of(&original));
    let Some(spliced) = splice_array_section(&original, name, &section) else {
        return Err(ConfigError::TableWrite {
            section: name.to_string(),
            path: path.to_path_buf(),
        });
    };
    if toml::from_str::<Config>(&spliced).is_err() {
        return Err(ConfigError::TableWrite {
            section: name.to_string(),
            path: path.to_path_buf(),
        });
    }
    write_atomic(path, &spliced)
}

/// Render an array-of-tables section.
///
/// WHY the helper: the nesting rule is two levels at most, and the
/// renderer was a loop in a loop (audit 2026-10-21 §F8).
///
/// `nl` is the document's own line ending, so a CRLF file stays a CRLF
/// file (audit 2026-10-23 §F-057).
fn render_array_table(name: &str, entries: &[Vec<(String, Option<String>)>], nl: &str) -> String {
    let mut out = String::new();
    for entry in entries {
        out.push_str(&render_array_entry(name, entry, nl));
    }
    out
}

/// Render one `[[name]]` entry.
fn render_array_entry(name: &str, entry: &[(String, Option<String>)], nl: &str) -> String {
    let mut out = String::new();
    out.push_str("[[");
    out.push_str(name);
    out.push_str("]]");
    out.push_str(nl);
    for (key, value) in entry {
        let Some(value) = value else {
            continue;
        };
        out.push_str(key);
        out.push_str(" = ");
        out.push_str(value);
        out.push_str(nl);
    }
    out
}

/// Replace (or append) one array-of-tables section in the document.
fn splice_array_section(text: &str, name: &str, section: &str) -> Option<String> {
    if has_inline_key(text, name) {
        return None;
    }
    match find_array_section(text, name) {
        Some(range) => Some(replace_range(text, range, section)),
        None => Some(append_section(text, section)),
    }
}

/// The byte range of the `[[name]]` section in `text`.
///
/// The range starts at the first `[[name]]` header and ends where the
/// next table header of any kind begins (or at the end of the text).
fn find_array_section(text: &str, name: &str) -> Option<Range<usize>> {
    let mut start: Option<usize> = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if is_any_header(trimmed) {
            let is_wanted = is_array_header(trimmed) && array_header_name(trimmed) == Some(name);
            if is_wanted && start.is_none() {
                start = Some(offset);
            } else if start.is_some() {
                return Some(start.unwrap_or(0)..offset);
            }
        }
        offset += line.len();
    }
    start.map(|begin| begin..text.len())
}

/// Is this line the header of an array of tables?
fn is_array_header(trimmed: &str) -> bool {
    let head = strip_trailing_comment(trimmed).trim_end();
    head.starts_with("[[") && head.ends_with("]]")
}

/// The name an array-of-tables header declares.
fn array_header_name(trimmed: &str) -> Option<&str> {
    let head = strip_trailing_comment(trimmed).trim_end();
    let inner = head.strip_prefix("[[")?.strip_suffix("]]")?;
    Some(unquote(inner.trim()))
}

/// Write one table's keys into the config, **in place**.
///
/// Every key is spliced **individually** — an existing key's line is
/// replaced where it stands, a missing one is appended inside the table
/// (the header is created once when the whole table is missing) — so the
/// user's comments and every key the app does not know survive **inside
/// the table too**, not only outside it. Replacing the whole section
/// body dropped an in-section comment or unknown key for
/// `[sky]`/`[weather]`/`[message]`/`[grass]` when a preset applied
/// (audit 2026-10-22 §P2-20 / 2026-10-23 §F-052).
///
/// A `None` entry **clears** its key: the writers that use this API
/// describe a whole table (a preset's `[grass]`, say), so "no value"
/// means "the key is gone", not "keep the user's old one" — otherwise a
/// preset that carries no grass tile could never clear the user's.
///
/// All the splices land in one text pass and one atomic write: a preset
/// is still one user action, one transaction.
///
/// The write is refused when the result would not parse: a file the
/// surgery cannot handle is left exactly as it was. A file that does not
/// exist yet is created from the whole default config, the same fresh
/// path `write_assignments` has (audit 2026-10-23 §F-064).
pub fn write_table(
    path: &Path,
    name: &str,
    entries: &[(&str, Option<String>)],
) -> Result<(), ConfigError> {
    let original = read_or_default(path)?;
    let Some(spliced) = splice_table(&original, name, entries) else {
        return Err(ConfigError::TableWrite {
            section: name.to_string(),
            path: path.to_path_buf(),
        });
    };
    write_checked(path, name, spliced)
}

/// Splice one table's keys into the document, key by key.
///
/// `None` when any single splice cannot be handled: the write is then
/// refused as a whole, exactly like the old section replace was.
///
/// `pub(crate)`: the multi-table composers splice several tables into ONE
/// text and write once (audit 2026-10-23 §F-051), so they share this
/// primitive instead of re-implementing the per-key surgery.
pub(crate) fn splice_table(
    text: &str,
    name: &str,
    entries: &[(&str, Option<String>)],
) -> Option<String> {
    let mut out = text.to_string();
    for (key, value) in entries {
        out = splice_entry(&out, name, key, value.as_deref())?;
    }
    Some(out)
}

/// Splice one entry: a value replaces (or appends) the key, `None`
/// removes it.
fn splice_entry(text: &str, section: &str, key: &str, value: Option<&str>) -> Option<String> {
    match value {
        Some(value) => splice_key(text, section, key, value),
        None => remove_key(text, section, key),
    }
}

/// Remove `key` from `section`; an absent key leaves the text as it was.
///
/// The whole assignment line goes — key, value, comment and newline — so
/// no half-line survives. A multi-line value is removed with all of its
/// continuation lines (the span covers them).
fn remove_key(text: &str, section: &str, key: &str) -> Option<String> {
    let Some(span) = find_value_span(text, section, key) else {
        // An absent key is a no-op — unless the file carries the whole
        // section as a top-level inline table, which a `None` entry
        // cannot reach into: refusing keeps the look from landing
        // half-applied (audit 2026-10-23 §F-045).
        if has_inline_key(text, section) {
            return None;
        }
        return Some(text.to_string());
    };
    let span = whole_line_span(text, span);
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..span.start]);
    out.push_str(&text[span.end..]);
    Some(out)
}

/// The byte range of the whole line(s) an assignment occupies.
fn whole_line_span(text: &str, span: Range<usize>) -> Range<usize> {
    let start = text[..span.start].rfind('\n').map_or(0, |i| i + 1);
    let end = text[span.end..]
        .find('\n')
        .map_or(text.len(), |i| span.end + i + 1);
    start..end
}

/// Parse-check a spliced document and write it, atomically.
fn write_checked(path: &Path, name: &str, spliced: String) -> Result<(), ConfigError> {
    if toml::from_str::<Config>(&spliced).is_err() {
        return Err(ConfigError::TableWrite {
            section: name.to_string(),
            path: path.to_path_buf(),
        });
    }
    write_atomic(path, &spliced)
}

/// A spliced document that still parses, or `None`.
///
/// `pub(crate)` for the multi-table composers: they splice several tables
/// into one text and parse-check the **finished** text once (audit
/// 2026-10-23 §F-051).
pub(crate) fn checked_config_text(text: String) -> Option<String> {
    toml::from_str::<Config>(&text).is_ok().then_some(text)
}

/// The `[assignments]` splice the whole-table writers use, with its
/// verification, as an in-memory primitive (audit 2026-10-23 §F-051).
///
/// `None` when the document carries an inline `assignments = …` key or the
/// spliced table would not read back equal — a composer then refuses the
/// whole edit rather than falling back to a whole-file rewrite, which
/// would eat the user's comments mid-preset.
pub(crate) fn splice_assignments_checked(text: &str, table: &AssignmentsConfig) -> Option<String> {
    let spliced = splice_assignments(text, table)?;
    assignments_survive(&spliced, table).then_some(spliced)
}

/// The array-of-tables splice, as an in-memory primitive.
///
/// `None` when the document carries an inline `<name> = …` key (a section
/// cannot replace it).
pub(crate) fn splice_array_table(
    text: &str,
    name: &str,
    entries: &[Vec<(String, Option<String>)>],
) -> Option<String> {
    let section = render_array_table(name, entries, newline_of(text));
    splice_array_section(text, name, &section)
}

/// Write ONE scalar key into the config at `path`, in place.
///
/// The key's value is replaced where it stands, so the user's comments,
/// their spacing and every key the app does not know survive. A key that
/// is not there yet is appended to its table (and the table is created
/// when it is missing too).
///
/// The write is refused when the result would not parse: a file the
/// surgery cannot handle is left exactly as it was. A file that does not
/// exist yet is created from the whole default config, the same fresh
/// path `write_assignments` has (audit 2026-10-23 §F-064).
///
/// WHY: the settings panel must not be able to destroy the user's
/// config the way the old whole-file rewrite could (audit 2026-10-20
/// §3.9) — the same discipline the `[assignments]` table write follows
/// (roadmap Extension 2 §3.2). The value arrives **already rendered**
/// as TOML (`30`, `true`, `"bottom-right"`), so this function never
/// adds quotes of its own.
pub fn write_key(path: &Path, section: &str, key: &str, value: &str) -> Result<(), ConfigError> {
    let original = read_or_default(path)?;
    // A single-key write into an array-of-tables name cannot say which
    // entry it means, and silently editing the first one is how the
    // §F1-class bugs used to look (audit 2026-10-23 §F-060). An indexed
    // key (`#0.name`) still lands on the entry it names; a whole array
    // table goes through `write_array_table`.
    if !key.starts_with('#') && names_array_block(&original, section) {
        return Err(ConfigError::TableWrite {
            section: section.to_string(),
            path: path.to_path_buf(),
        });
    }
    let Some(spliced) = splice_key(&original, section, key, value) else {
        return Err(ConfigError::TableWrite {
            section: section.to_string(),
            path: path.to_path_buf(),
        });
    };
    write_checked(path, section, spliced)
}

/// Does the document carry a `[[section]]` block?
fn names_array_block(text: &str, section: &str) -> bool {
    text.split_inclusive('\n')
        .any(|line| array_header_name(line.trim()) == Some(section))
}

/// Splice one scalar key into the document.
///
/// `None` when the surgery cannot handle the document: an inline
/// `section = …` key cannot be replaced by a table.
fn splice_key(text: &str, section: &str, key: &str, value: &str) -> Option<String> {
    if let Some(replaced) = replace_value(text, section, key, value) {
        return Some(replaced);
    }
    if section.is_empty() {
        return Some(append_top_level_key(text, key, value));
    }
    if find_section(text, section).is_some() {
        return append_key(text, section, key, value);
    }
    if has_inline_key(text, section) {
        return None;
    }
    Some(append_section(
        text,
        &render_table(section, &[(key, Some(value.to_string()))], newline_of(text)),
    ))
}

/// Append `key = value` to the end of the `[section]` table in `text`.
///
/// The key lands after the table's last own line, so it stays inside the
/// table and the blank line the user put before the next table header
/// keeps its place.
fn append_key(text: &str, section: &str, key: &str, value: &str) -> Option<String> {
    let range = find_section(text, section)?;
    let offset = range.start + insert_after_last_content(&text[range.start..range.end]);
    Some(insert_line(text, offset, &render_key_line(key, value)))
}

/// Append `key = value` to the top level of the document.
///
/// A top-level key may not live after a table header — in TOML the
/// header would swallow it into that table — so the key goes **before**
/// the first one. A document without a table header takes the key at its
/// end.
fn append_top_level_key(text: &str, key: &str, value: &str) -> String {
    let end = first_table_offset(text).unwrap_or(text.len());
    let offset = insert_after_last_content(&text[..end]);
    insert_line(text, offset, &render_key_line(key, value))
}

/// The byte offset of the first table header in `text`.
///
/// `None` when the document has no table at all — the top level is then
/// the whole document.
///
/// A `[[array]]` header counts too: a top-level key may not live after
/// it either, or the array block would swallow it (audit 2026-10-21 §F1).
fn first_table_offset(text: &str) -> Option<usize> {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if is_any_header(line.trim()) {
            return Some(offset);
        }
        offset += line.len();
    }
    None
}

/// The offset just after the last line of `block` that carries content.
///
/// The blank lines the user put before the next table header keep their
/// place: the new key lands right after the last real line, not after
/// the blank ones.
fn insert_after_last_content(block: &str) -> usize {
    let mut offset = 0;
    let mut insert = 0;
    for line in block.split_inclusive('\n') {
        offset += line.len();
        if !line.trim().is_empty() {
            insert = offset;
        }
    }
    insert
}

/// Insert a rendered `key = value` line into `text` at `offset`.
///
/// The line starts on its own line: a text that does not end with a
/// newline at that point gets one, so the key can never be glued to the
/// line before it. The inserted newline is the document's own ending, so
/// a CRLF file stays a CRLF file (audit 2026-10-23 §F-057).
fn insert_line(text: &str, offset: usize, line: &str) -> String {
    let nl = newline_of(text);
    let mut out = String::with_capacity(text.len() + line.len() + 2);
    out.push_str(&text[..offset]);
    if offset > 0 && !text[..offset].ends_with('\n') {
        out.push_str(nl);
    }
    out.push_str(line);
    out.push_str(nl);
    out.push_str(&text[offset..]);
    out
}

/// Render one `key = value` line.
fn render_key_line(key: &str, value: &str) -> String {
    let mut line = String::with_capacity(key.len() + value.len() + 4);
    line.push_str(key);
    line.push_str(" = ");
    line.push_str(value);
    line
}

/// Write the values the repair pass changed back into the config.
///
/// Each repair replaces the value of one key **in place**, so the user's
/// comments and unknown keys survive. The write happens only when every
/// repair could be applied and the result still parses — a file the
/// surgery cannot handle is left exactly as it was.
pub fn write_repairs(path: &Path, repairs: &[Repair]) -> Result<(), ConfigError> {
    if repairs.is_empty() || !path.exists() {
        return Ok(());
    }
    let original = read_to_string(path)?;
    let Some(repaired) = apply_repairs(&original, repairs) else {
        // Silent skips hid a repair that never landed: the app ran on the
        // fixed value, the file kept the broken one, and the warning
        // repeated on every load with nothing to look at
        // (audit 2026-10-23 §F-053).
        crate::trace::warn(&format!(
            "config: {} repair(s) could not be spliced into {} — the file keeps the old values",
            repairs.len(),
            path.display()
        ));
        return Ok(());
    };
    if toml::from_str::<Config>(&repaired).is_err() {
        crate::trace::warn(&format!(
            "config: the repaired {} would not parse — the file keeps the old values",
            path.display()
        ));
        return Ok(());
    }
    write_atomic(path, &repaired)
}

/// Apply every repair to the document, or `None` when one of them does
/// not apply.
///
/// A repair that cannot be applied aborts the whole write: a half-repaired
/// config would leave the app and the file disagreeing.
fn apply_repairs(text: &str, repairs: &[Repair]) -> Option<String> {
    let mut out = text.to_string();
    for repair in repairs {
        out = replace_value(&out, repair.section, &repair.key, &repair.value)?;
    }
    Some(out)
}

/// Replace the value of `key` inside `section` in `text`.
///
/// `section` is the table name; `""` is the top level. `None` when the
/// key is not there, or when its value spans several lines and cannot be
/// replaced safely.
///
/// The `key = ` prefix is left alone — only the value is swapped — so the
/// key's spelling and the user's spacing survive.
fn replace_value(text: &str, section: &str, key: &str, value: &str) -> Option<String> {
    let span = find_value_span(text, section, key)?;
    let mut out = String::with_capacity(text.len() + value.len());
    out.push_str(&text[..span.start]);
    out.push(' ');
    out.push_str(value);
    out.push_str(&text[span.end..]);
    Some(out)
}

/// The byte range of the *value* of `key` inside `section`.
///
/// The range covers the value only — the `key = ` prefix stays, so the
/// key's spelling and spacing survive. A value that spans several lines
/// (an array, say) is covered whole, so the replacement cannot leave a
/// dangling continuation.
///
/// The scope of a line is tracked as a [`Scope`], so a key that lives
/// inside a `[[array]]` block is **not** attributed to the table that
/// came before it (audit 2026-10-21 §F1).
fn find_value_span(text: &str, section: &str, key: &str) -> Option<Range<usize>> {
    let (entry, field) = split_entry_key(key);
    let mut scope = Scope::Top;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if is_any_header(trimmed) {
            scope = scope_of_header(trimmed, &scope);
            offset += line.len();
            continue;
        }
        if line_matches_scope(&scope, section, entry) && line_key(trimmed) == Some(field) {
            return Some(value_span(text, offset, line));
        }
        offset += line.len();
    }
    None
}

/// The part of the document a line belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Scope {
    /// The top level: the keys before the first table header.
    Top,
    /// A `[name]` table.
    Table(String),
    /// A `[[name]]` array of tables: the **entry** of the block the line
    /// belongs to, counted from the block's first header.
    ///
    /// The entry matters: a repair of a `[[personality]]` entry addresses
    /// **one** entry, not the first one (audit 2026-10-21 §E4).
    Array { name: String, entry: usize },
}

/// The scope a header line opens.
///
/// The previous scope is the one the header **replaces**: two
/// `[[name]]` headers in a row are two entries of the same block, so the
/// second one is the next entry.
fn scope_of_header(trimmed: &str, previous: &Scope) -> Scope {
    if is_array_header(trimmed) {
        let name = array_header_name(trimmed).unwrap_or_default().to_string();
        let entry = match previous {
            Scope::Array {
                name: previous_name,
                entry,
            } if *previous_name == name => entry + 1,
            _ => 0,
        };
        return Scope::Array { name, entry };
    }
    Scope::Table(header_name(trimmed).unwrap_or_default().to_string())
}

/// Does this line's scope hold the key being sought?
///
/// WHY a helper of its own: the decision is one rule with an exception.
/// An un-indexed key ("the `name` of `personality`") belongs to the
/// array block it sits in — the **first** entry, in scan order — and
/// never to the `[table]` that came before it (audit 2026-10-21 §F1).
/// An entry is addressed directly through its explicit index
/// (`#2.bob`); an un-indexed **write** into an array is refused in
/// [`write_key`], which is the trap §F-060 closed.
fn line_matches_scope(scope: &Scope, section: &str, entry: Option<usize>) -> bool {
    match scope {
        Scope::Array { name, .. } => name == section && entry_matches(scope, entry),
        _ => scope_holds(scope, section) && entry_matches(scope, entry),
    }
}

/// Does this scope hold the keys of the `[section]` table?
///
/// The top level is the empty section name. An array block answers
/// **false** here — it matches only through its own arm in
/// [`line_matches_scope`], so a key inside a `[[personality]]` block is
/// never attributed to the `[table]` that came before it
/// (audit 2026-10-21 §F1).
fn scope_holds(scope: &Scope, section: &str) -> bool {
    match scope {
        Scope::Top => section.is_empty(),
        Scope::Table(name) => name == section,
        Scope::Array { .. } => false,
    }
}

/// The entry of an array a key addresses, and the key itself.
///
/// The form is `#<index>.<field>`: the third entry of the block, its
/// `bob` key. A key without the form addresses the **first** entry (or
/// the table's own key).
fn split_entry_key(key: &str) -> (Option<usize>, &str) {
    let Some(rest) = key.strip_prefix('#') else {
        return (None, key);
    };
    let Some((index, field)) = rest.split_once('.') else {
        return (None, key);
    };
    match index.parse::<usize>() {
        Ok(index) => (Some(index), field),
        Err(_) => (None, key),
    }
}

/// Is this the entry the key addresses?
fn entry_matches(scope: &Scope, entry: Option<usize>) -> bool {
    match (scope, entry) {
        (Scope::Array { entry: current, .. }, Some(wanted)) => *current == wanted,
        (_, None) => true,
        (_, Some(_)) => false,
    }
}

/// The key a line assigns to, or `None` when the line is not an
/// assignment.
fn line_key(trimmed: &str) -> Option<&str> {
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let (key, _) = trimmed.split_once('=')?;
    Some(unquote(key.trim()))
}

/// The byte range of the value that starts on the line at `line_start`.
///
/// A multi-line value (an array written across several lines) is covered
/// whole: the brackets are counted, and the strings are skipped so a
/// bracket inside a quoted string does not confuse the count.
fn value_span(text: &str, line_start: usize, line: &str) -> Range<usize> {
    let Some(eq) = line.find('=') else {
        return line_start..line_start + line.len();
    };
    let value_start = line_start + eq + 1;
    let mut depth = bracket_depth(&line[eq + 1..]);
    if depth == 0 {
        return value_start..value_start + value_len_to_end_of_line(&line[eq + 1..]);
    }
    // The value runs over several lines: consume them until the brackets
    // balance. The span ends at the last line's **content**, so the
    // newline that ends it is not swallowed.
    let mut end = line_start + line.len();
    let mut content_end = end;
    for next in text[line_start + line.len()..].split_inclusive('\n') {
        depth += bracket_depth(next);
        end += next.len();
        let content = next.trim_end_matches(['\n', '\r']);
        content_end = end - (next.len() - content.len());
        if depth <= 0 {
            break;
        }
    }
    value_start..content_end
}

/// The number of unclosed brackets and braces in a fragment.
///
/// A `#` outside a string starts a **comment** and ends the scan: the
/// rest of the line is not TOML, and a bracket inside it used to be
/// counted — so `key = 1 # [` looked like the start of a multi-line
/// value, the span swallowed the lines after it, and the splice landed
/// somewhere else entirely (audit 2026-10-23 §F-047).
fn bracket_depth(fragment: &str) -> i32 {
    let mut depth = 0;
    let mut in_string = false;
    let mut in_literal = false;
    let mut escaped = false;
    for ch in fragment.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if in_literal {
            if ch == '\'' {
                in_literal = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '\'' => in_literal = true,
            '#' => break,
            '[' | '{' => depth += 1,
            ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    depth
}

/// The length of a value up to the end of its line, without the trailing
/// newline and without a trailing comment.
fn value_len_to_end_of_line(value: &str) -> usize {
    let without_newline = value.trim_end_matches(['\n', '\r']);
    let without_comment = strip_trailing_comment(without_newline);
    without_comment.len()
}

/// Drop a trailing `# comment` from a value, respecting quoted strings.
fn strip_trailing_comment(value: &str) -> &str {
    let mut in_string = false;
    let mut escaped = false;
    for (index, ch) in value.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
            continue;
        }
        if ch == '#' {
            return value[..index].trim_end();
        }
    }
    value.trim_end()
}

/// Read a file into a string, reporting the path on failure.
fn read_to_string(path: &Path) -> Result<String, ConfigError> {
    std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Write a config file that does not exist yet: the whole default config
/// carries the table, because there is nothing of the user's to preserve.
fn write_fresh_config(path: &Path, table: &AssignmentsConfig) -> Result<(), ConfigError> {
    let cfg = Config {
        assignments: table.clone(),
        ..Config::default()
    };
    cfg.save_to(path)
}

/// The fallback write: the whole file is rewritten from the struct.
///
/// Used when the document is not in a shape the surgical edit can handle.
/// The config is loaded **unsanitized**, so the repair pass cannot drop
/// the assignments whose asset does not exist (yet).
fn write_whole_config(path: &Path, table: &AssignmentsConfig) -> Result<(), ConfigError> {
    let mut cfg = Config::load_unsanitized_from(path)?;
    cfg.assignments = table.clone();
    cfg.save_to(path)
}

/// Replace the `[assignments]` section of `text` with the rendered table.
///
/// `None` when the document carries an inline `assignments = …` key, which
/// a section cannot replace.
fn splice_assignments(text: &str, table: &AssignmentsConfig) -> Option<String> {
    if has_inline_assignments(text) {
        return None;
    }
    let section = render_assignments(text, table);
    match find_assignments_section(text) {
        Some(range) => Some(replace_range(text, range, &section)),
        None => Some(append_section(text, &section)),
    }
}

/// The spliced document must parse and must carry exactly the table that
/// was meant to be written.
fn assignments_survive(text: &str, table: &AssignmentsConfig) -> bool {
    let Ok(cfg) = toml::from_str::<Config>(text) else {
        return false;
    };
    cfg.assignments == *table
}

/// The document's dominant line ending.
///
/// A CRLF file must stay a CRLF file: the surgical writers used to emit
/// bare `\n` for every line they touched, so one settings edit turned a
/// Windows config into a mixed-ending file that every diff tool and
/// editor flags (audit 2026-10-23 §F-057). The ending is detected once,
/// from the whole document, and every **new** line the writers add uses
/// it.
fn newline_of(text: &str) -> &'static str {
    let newlines = text.matches('\n').count();
    let crlf = text.matches("\r\n").count();
    if crlf * 2 > newlines {
        "\r\n"
    } else {
        "\n"
    }
}

/// Render the `[assignments]` section.
fn render_assignments(text: &str, table: &AssignmentsConfig) -> String {
    let entries: Vec<(String, Option<String>)> = table
        .entries()
        .into_iter()
        .map(|(key, value)| {
            let rendered = value.map(|v| toml::Value::String(v.to_string()).to_string());
            (key, rendered)
        })
        .collect();
    render_table("assignments", &entries, newline_of(text))
}

/// Render a table's section: the header and one line per entry.
///
/// An entry with no value is **omitted** — an unset key is not written
/// as an empty string, so the file stays readable. `nl` is the
/// document's own line ending (audit 2026-10-23 §F-057).
fn render_table<K: AsRef<str>>(name: &str, entries: &[(K, Option<String>)], nl: &str) -> String {
    let mut out = String::from("[");
    out.push_str(name);
    out.push(']');
    out.push_str(nl);
    for (key, value) in entries {
        let Some(value) = value else {
            continue;
        };
        out.push_str(key.as_ref());
        out.push_str(" = ");
        out.push_str(value);
        out.push_str(nl);
    }
    out
}

/// The byte range of the `[assignments]` section in `text`.
///
/// The range starts at the header line and ends where the next table
/// header begins (or at the end of the text). `None` when the document
/// has no such section.
fn find_assignments_section(text: &str) -> Option<Range<usize>> {
    find_section(text, "assignments")
}

/// The byte range of the `[name]` section in `text`.
///
/// The range starts at the header line and ends where the next **table
/// header of any kind** begins — a `[table]` *and* a `[[array]]` both
/// terminate it — or at the end of the text. `None` when the document
/// has no such section.
///
/// WHY the array header terminates the range too: a table that is
/// followed by an array of tables used to swallow the whole array block
/// (the range ran to the end of the file), so a key the settings panel
/// appended "to the table" landed **inside** the array block. The
/// `Config` struct ignores the unknown field, so the validation parse
/// succeeded and the corruption was written to the user's file
/// (audit 2026-10-21 §F1).
fn find_section(text: &str, name: &str) -> Option<Range<usize>> {
    let mut start: Option<usize> = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if is_any_header(trimmed) {
            let is_wanted = is_table_header(trimmed) && header_name(trimmed) == Some(name);
            if is_wanted && start.is_none() {
                start = Some(offset);
            } else if start.is_some() {
                return Some(start.unwrap_or(0)..offset);
            }
        }
        offset += line.len();
    }
    start.map(|begin| begin..text.len())
}

/// Is this line the header of a table (not an array of tables)?
fn is_table_header(trimmed: &str) -> bool {
    let head = strip_trailing_comment(trimmed).trim_end();
    head.starts_with('[') && !head.starts_with("[[")
}

/// Is this line the header of a table **or** of an array of tables?
///
/// A `[[array]]` block ends a `[table]` section just as a `[table]`
/// header does — see [`find_section`].
fn is_any_header(trimmed: &str) -> bool {
    is_table_header(trimmed) || is_array_header(trimmed)
}

/// The name a table header declares, with the quotes stripped.
///
/// A header may carry a **trailing comment** (`[sky] # dawn`). The
/// comment is not part of the name, and a header that ends in a comment
/// is still a header: without this the surgical writer appended a
/// duplicate table and the settings panel could never store that key
/// (audit 2026-10-22 §P1-8).
fn header_name(trimmed: &str) -> Option<&str> {
    let head = strip_trailing_comment(trimmed).trim_end();
    let inner = head.strip_prefix('[')?.strip_suffix(']')?;
    Some(unquote(inner.trim()))
}

/// Strip the quotes a TOML key may carry.
fn unquote(key: &str) -> &str {
    let quoted = key.len() >= 2 && key.starts_with('"') && key.ends_with('"');
    if quoted {
        return &key[1..key.len() - 1];
    }
    key
}

/// Does the document carry an inline `assignments = …` key?
///
/// Only the lines **before** the first table header are inspected: in
/// TOML the top-level keys must come first, so anything after a header
/// belongs to a table.
fn has_inline_assignments(text: &str) -> bool {
    has_inline_key(text, "assignments")
}

/// Does the document carry an inline `<name> = …` key?
///
/// Only the lines **before the first header of any kind** are inspected:
/// in TOML the top-level keys must come first, so anything after a
/// `[table]` **or** a `[[array]]` header belongs to that block. A key
/// inside an array block used to be mistaken for a top-level inline key,
/// so the surgical write was refused (audit 2026-10-21 §F1).
fn has_inline_key(text: &str, name: &str) -> bool {
    for line in text.lines() {
        let trimmed = line.trim();
        if is_any_header(trimmed) {
            return false;
        }
        if trimmed.starts_with('#') {
            continue;
        }
        let Some((key, _)) = trimmed.split_once('=') else {
            continue;
        };
        if unquote(key.trim()) == name {
            return true;
        }
    }
    false
}

/// Replace a byte range of `text` with `section`.
fn replace_range(text: &str, range: Range<usize>, section: &str) -> String {
    let mut out = String::with_capacity(text.len() + section.len());
    out.push_str(&text[..range.start]);
    out.push_str(section);
    out.push_str(&text[range.end..]);
    out
}

/// Append a section to the end of the document.
///
/// The separator newline is the document's own ending, so a CRLF file
/// stays a CRLF file (audit 2026-10-23 §F-057).
fn append_section(text: &str, section: &str) -> String {
    let nl = newline_of(text);
    let mut out = String::with_capacity(text.len() + section.len() + 2);
    out.push_str(text);
    if !out.is_empty() && !out.ends_with('\n') {
        out.push_str(nl);
    }
    if !out.is_empty() {
        out.push_str(nl);
    }
    out.push_str(section);
    out
}

/// Write `text` to `path` atomically, keeping a backup of the old file.
///
/// WHY atomic: `std::fs::write` truncates the file first, so a crash or a
/// power loss mid-write left a truncated `config.toml` — and a truncated
/// config fails to parse, which bricked every config-reading command with
/// no way back (audit 2026-10-20 §3.9). Writing a temp file and renaming
/// it over the target is atomic on every platform the app supports, so the
/// old file is either fully there or fully replaced.
///
/// The backup is best effort: a failure to copy must not stop the write.
pub(crate) fn write_atomic(path: &Path, text: &str) -> Result<(), ConfigError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let temp = temp_path(path);
    std::fs::write(&temp, text).map_err(|source| ConfigError::Io {
        path: temp.clone(),
        source,
    })?;
    if path.exists() {
        keep_backup(path);
    }
    match std::fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(source) => {
            let _ = std::fs::remove_file(&temp);
            Err(ConfigError::Io {
                path: path.to_path_buf(),
                source,
            })
        }
    }
}

/// The temp file a write goes through: a sibling of the target, named
/// after it and the writing process, so two writers do not collide.
fn temp_path(path: &Path) -> std::path::PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "config.toml".to_string());
    sibling(path, &format!("{name}.{}.tmp", std::process::id()))
}

/// The backup file the write keeps next to the target.
fn backup_path(path: &Path) -> std::path::PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "config.toml".to_string());
    sibling(path, &format!("{name}{BACKUP_SUFFIX}"))
}

/// A sibling path of `path` with the given file name.
fn sibling(path: &Path, name: &str) -> std::path::PathBuf {
    match path.parent() {
        Some(parent) => parent.join(name),
        None => std::path::PathBuf::from(name),
    }
}

/// Copy the current file to its backup slot.
///
/// Best effort: a backup that cannot be written is worth a warning, not a
/// failed config write.
fn keep_backup(path: &Path) {
    let backup = backup_path(path);
    if let Err(e) = std::fs::copy(path, &backup) {
        crate::trace::warn(&format!(
            "config: could not write the backup {}: {e}",
            backup.display()
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AssignmentSlot, HintCorner};

    /// A table with one assignment set.
    fn table_with(name: &str) -> AssignmentsConfig {
        let mut table = AssignmentsConfig::default();
        table.set(
            AssignmentSlot::parse("small_3").expect("small_3"),
            Some(name.into()),
        );
        table
    }

    /// A config file with a comment, an unknown key and a table.
    const SAMPLE: &str = "# my ducks\nloop_seconds = 12.0\nmy_own_key = \"keep me\"\n\n[balloons]\nmax = 5\n\n[assignments]\nbig_1 = \"cat\"\n";

    /// A temp dir and a config path inside it.
    ///
    /// The tests never touch the user's real config file: every write
    /// goes into a temp dir that is removed again at the end.
    fn temp_config(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("agnosis-config-edit-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("config.toml");
        (dir, path)
    }

    #[test]
    fn the_splice_keeps_the_comments_and_the_unknown_keys() {
        let table = table_with("dog");
        let spliced = splice_assignments(SAMPLE, &table).expect("the sample is canonical");
        assert!(
            spliced.contains("# my ducks"),
            "the comment must survive: {spliced}"
        );
        assert!(
            spliced.contains("my_own_key = \"keep me\""),
            "an unknown key must survive: {spliced}"
        );
        assert!(
            spliced.contains("loop_seconds = 12.0"),
            "another table must survive: {spliced}"
        );
        assert!(
            spliced.contains("[balloons]"),
            "the balloons table must survive: {spliced}"
        );
        assert!(
            spliced.contains("small_3 = \"dog\""),
            "the new assignment must be there: {spliced}"
        );
        assert!(
            !spliced.contains("big_1 = \"cat\""),
            "the old assignment must be gone: {spliced}"
        );
    }

    #[test]
    fn the_spliced_document_still_parses() {
        let table = table_with("dog");
        let spliced = splice_assignments(SAMPLE, &table).expect("the sample is canonical");
        let cfg: Config = toml::from_str(&spliced).expect("the spliced document must parse");
        assert_eq!(cfg.assignments, table);
        assert_eq!(cfg.loop_seconds, 12.0);
    }

    #[test]
    fn an_empty_table_renders_an_empty_section() {
        let table = AssignmentsConfig::default();
        let spliced = splice_assignments(SAMPLE, &table).expect("the sample is canonical");
        assert!(spliced.contains("[assignments]"));
        assert!(!spliced.contains("big_1"));
        let cfg: Config = toml::from_str(&spliced).expect("the spliced document must parse");
        assert_eq!(cfg.assignments, table);
    }

    #[test]
    fn a_document_without_the_section_gets_one_appended() {
        let text = "loop_seconds = 12.0\n";
        let table = table_with("dog");
        let spliced = splice_assignments(text, &table).expect("the document is canonical");
        assert!(spliced.contains("[assignments]"));
        assert!(spliced.contains("small_3 = \"dog\""));
        let cfg: Config = toml::from_str(&spliced).expect("the spliced document must parse");
        assert_eq!(cfg.assignments, table);
    }

    #[test]
    fn an_inline_assignments_key_is_refused() {
        let text = "assignments = { small_3 = \"dog\" }\n";
        assert!(
            splice_assignments(text, &table_with("dog")).is_none(),
            "an inline table cannot be replaced by a section"
        );
    }

    #[test]
    fn the_section_ends_at_the_next_table() {
        let text = "[assignments]\nsmall_1 = \"a\"\n\n[hotkey]\neditor = \"D+Shift\"\n";
        let table = table_with("dog");
        let spliced = splice_assignments(text, &table).expect("the document is canonical");
        assert!(spliced.contains("small_3 = \"dog\""));
        assert!(
            spliced.contains("[hotkey]\neditor = \"D+Shift\""),
            "the next table must survive: {spliced}"
        );
        assert!(!spliced.contains("small_1 = \"a\""));
    }

    #[test]
    fn the_write_keeps_the_users_file_and_leaves_a_backup() {
        let dir = std::env::temp_dir().join("agnosis-config-edit-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("config.toml");
        std::fs::write(&path, SAMPLE).expect("write the sample");

        let table = table_with("dog");
        write_assignments(&path, &table).expect("the write succeeds");

        let after = std::fs::read_to_string(&path).expect("read back");
        assert!(
            after.contains("# my ducks"),
            "the comment survives: {after}"
        );
        assert!(
            after.contains("my_own_key"),
            "the unknown key survives: {after}"
        );
        assert!(
            after.contains("small_3 = \"dog\""),
            "the assignment lands: {after}"
        );

        let backup = dir.join("config.toml.bak");
        assert!(backup.exists(), "the write keeps a backup");
        let old = std::fs::read_to_string(&backup).expect("read the backup");
        assert!(
            old.contains("big_1 = \"cat\""),
            "the backup is the old file: {old}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fresh_file_gets_the_whole_default_config() {
        let dir = std::env::temp_dir().join("agnosis-config-edit-fresh-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("config.toml");

        let table = table_with("dog");
        write_assignments(&path, &table).expect("the write succeeds");

        let after = std::fs::read_to_string(&path).expect("read back");
        assert!(after.contains("loop_seconds"), "the defaults land: {after}");
        assert!(after.contains("[balloons]"), "the tables land: {after}");
        assert!(
            after.contains("small_3 = \"dog\""),
            "the assignment lands: {after}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_write_never_leaves_a_temp_file_behind() {
        let dir = std::env::temp_dir().join("agnosis-config-edit-temp-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("config.toml");
        std::fs::write(&path, SAMPLE).expect("write the sample");

        write_assignments(&path, &table_with("dog")).expect("the write succeeds");

        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .expect("read the dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no temp file may survive: {leftovers:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A repair replaces the value of one key, in place.
    #[test]
    fn a_repair_replaces_the_value_in_place() {
        let text = "# mine\nmax_cols = 260\nloop_seconds = 12.0\n";
        let repaired = replace_value(text, "", "max_cols", "4096").expect("the key is there");
        assert!(
            repaired.contains("max_cols = 4096"),
            "the value lands: {repaired}"
        );
        assert!(
            repaired.contains("# mine"),
            "the comment survives: {repaired}"
        );
        assert!(
            repaired.contains("loop_seconds = 12.0"),
            "the other keys survive: {repaired}"
        );
    }

    /// A repair inside a table only touches that table.
    #[test]
    fn a_repair_inside_a_table_leaves_the_top_level_alone() {
        let repair = Repair {
            section: "balloons",
            key: "max".to_string(),
            value: "5".to_string(),
        };
        let text = "max = 99\n\n[balloons]\nmax = 64\n";
        let repaired =
            apply_repairs(text, std::slice::from_ref(&repair)).expect("the key is there");
        assert!(
            repaired.contains("[balloons]\nmax = 5"),
            "the table is repaired: {repaired}"
        );
        assert!(
            repaired.contains("max = 99"),
            "the top level is untouched: {repaired}"
        );
    }

    /// A multi-line value is replaced whole, so no dangling continuation
    /// is left behind.
    #[test]
    fn a_multi_line_value_is_replaced_whole() {
        let text = "cell_size = [\n    12,\n    24,\n]\nloop_seconds = 12.0\n";
        let repair = Repair {
            section: "",
            key: "cell_size".to_string(),
            value: "[12, 24]".to_string(),
        };
        let repaired =
            apply_repairs(text, std::slice::from_ref(&repair)).expect("the key is there");
        assert!(
            repaired.contains("cell_size = [12, 24]"),
            "the array is replaced: {repaired}"
        );
        assert!(
            !repaired.contains("    12,"),
            "no continuation survives: {repaired}"
        );
        let cfg: Config = toml::from_str(&repaired).expect("the result must parse");
        assert_eq!(cfg.cell_size, [12, 24]);
    }

    /// A repair that does not apply aborts the whole write: a
    /// half-repaired config would leave the app and the file disagreeing.
    #[test]
    fn a_repair_that_does_not_apply_aborts_the_write() {
        let repair = Repair {
            section: "",
            key: "not_a_key".to_string(),
            value: "1".to_string(),
        };
        let text = "loop_seconds = 12.0\n";
        assert!(
            apply_repairs(text, std::slice::from_ref(&repair)).is_none(),
            "an inapplicable repair must abort"
        );
    }

    /// The repair pass reports the values it changed, and the load writes
    /// them back so the warning does not repeat on every run.
    #[test]
    fn the_load_writes_the_repairs_back_into_the_file() {
        let dir = std::env::temp_dir().join("agnosis-config-repair-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("config.toml");
        std::fs::write(&path, "# mine\nmax_cols = 260\n").expect("write the sample");

        // The existence predicate is injected: the sample carries no
        // assignment, and the test is about the repaired `max_cols`, so
        // every name counts as present and nothing is dropped.
        let cfg = Config::load_from(&path, |_, _| true).expect("the load succeeds");
        assert_eq!(cfg.max_cols, 4096, "the value is repaired in memory");

        let after = std::fs::read_to_string(&path).expect("read back");
        assert!(
            after.contains("max_cols = 4096"),
            "the repair is written back: {after}"
        );
        assert!(after.contains("# mine"), "the comment survives: {after}");

        // A second load finds nothing to repair, so the file is untouched.
        let before = std::fs::read_to_string(&path).expect("read back");
        let _ = Config::load_from(&path, |_, _| true).expect("the second load succeeds");
        let after = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(before, after, "a clean config is not rewritten");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A key is replaced where it stands: only its value moves.
    #[test]
    fn a_key_is_replaced_in_place() {
        let (dir, path) = temp_config("key-replace");
        std::fs::write(&path, SAMPLE).expect("write the sample");

        write_key(&path, "balloons", "max", "9").expect("the write succeeds");

        let after = std::fs::read_to_string(&path).expect("read back");
        let expected = "# my ducks\nloop_seconds = 12.0\nmy_own_key = \"keep me\"\n\n[balloons]\nmax = 9\n\n[assignments]\nbig_1 = \"cat\"\n";
        assert_eq!(after, expected, "only the value changes: {after}");
        let cfg: Config = toml::from_str(&after).expect("the result must parse");
        assert_eq!(cfg.balloons.max, 9);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A key that is not there yet is appended to its own table.
    #[test]
    fn a_missing_key_is_appended_to_its_table() {
        let (dir, path) = temp_config("key-append");
        std::fs::write(&path, SAMPLE).expect("write the sample");

        write_key(&path, "balloons", "min", "2").expect("the write succeeds");

        let after = std::fs::read_to_string(&path).expect("read back");
        let table = after.find("[balloons]").expect("the table is there");
        let key = after.find("min = 2").expect("the key is there");
        let next = after
            .find("[assignments]")
            .expect("the next table is there");
        assert!(
            table < key && key < next,
            "the key lands inside the table: {after}"
        );
        let cfg: Config = toml::from_str(&after).expect("the result must parse");
        assert_eq!(cfg.balloons.max, 5, "the table keeps its own keys: {after}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A header that carries a trailing comment is still that table's
    /// header.
    ///
    /// Without this the surgical writer did not recognise `[sky] # dawn`,
    /// appended a **duplicate** `[sky]` table, the parse check refused
    /// the result, and the settings panel could never store that key
    /// (audit 2026-10-22 §P1-8).
    #[test]
    fn a_header_with_a_trailing_comment_is_recognised() {
        let (dir, path) = temp_config("key-header-comment");
        std::fs::write(&path, "# my ducks\n\n[sky] # dawn\nenabled = false\n")
            .expect("write the sample");

        write_key(&path, "sky", "enabled", "true").expect("the write succeeds");

        let after = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(
            after.matches("[sky]").count(),
            1,
            "the table is not duplicated: {after}"
        );
        assert!(
            after.contains("[sky] # dawn"),
            "the comment on the header survives: {after}"
        );
        let cfg: Config = toml::from_str(&after).expect("the result must parse");
        assert!(cfg.sky.enabled, "the value lands: {after}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A table that is not there yet is created at the end.
    #[test]
    fn a_missing_table_is_created() {
        let (dir, path) = temp_config("key-new-table");
        std::fs::write(&path, SAMPLE).expect("write the sample");

        write_key(&path, "sky", "enabled", "true").expect("the write succeeds");

        let after = std::fs::read_to_string(&path).expect("read back");
        assert!(
            after.contains("[sky]\nenabled = true\n"),
            "the table is created: {after}"
        );
        assert!(
            after.contains("# my ducks"),
            "the comment survives: {after}"
        );
        assert!(
            after.contains("my_own_key = \"keep me\""),
            "the unknown key survives: {after}"
        );
        let cfg: Config = toml::from_str(&after).expect("the result must parse");
        assert!(cfg.sky.enabled, "the value lands: {after}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A top-level key is inserted before the first table header.
    #[test]
    fn a_top_level_key_is_appended_before_the_first_table() {
        let (dir, path) = temp_config("key-top-level");
        std::fs::write(&path, SAMPLE).expect("write the sample");

        write_key(&path, "", "hint_corner", "\"bottom-right\"").expect("the write succeeds");

        let after = std::fs::read_to_string(&path).expect("read back");
        let key = after.find("hint_corner").expect("the key is there");
        let table = after.find("[balloons]").expect("the table is there");
        assert!(
            key < table,
            "a top-level key may not live after a table header: {after}"
        );
        let cfg: Config = toml::from_str(&after).expect("the result must parse");
        assert_eq!(cfg.hint_corner, HintCorner::BottomRight);
        assert!(
            after.contains("# my ducks"),
            "the comment survives: {after}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A value the config cannot hold is refused: the file is left alone.
    #[test]
    fn a_write_that_would_not_parse_is_refused() {
        let (dir, path) = temp_config("key-bad-value");
        std::fs::write(&path, SAMPLE).expect("write the sample");
        let before = std::fs::read(&path).expect("read the file");

        let err = write_key(&path, "", "loop_seconds", "\"not a number\"")
            .expect_err("a value the config cannot hold is refused");
        assert!(
            matches!(&err, ConfigError::TableWrite { .. }),
            "the refusal is a table write: {err}"
        );

        let after = std::fs::read(&path).expect("read the file");
        assert_eq!(before, after, "the file is left exactly as it was");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An inline table cannot be replaced by a section: it is refused.
    #[test]
    fn an_inline_table_is_refused() {
        let (dir, path) = temp_config("key-inline");
        let text = "# mine\nmy_thing = { a = 1 }\nloop_seconds = 12.0\n";
        std::fs::write(&path, text).expect("write the sample");
        let before = std::fs::read(&path).expect("read the file");

        let err = write_key(&path, "my_thing", "b", "2").expect_err("an inline table is refused");
        assert!(
            matches!(&err, ConfigError::TableWrite { .. }),
            "the refusal is a table write: {err}"
        );

        let after = std::fs::read(&path).expect("read the file");
        assert_eq!(before, after, "the file is left exactly as it was");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A successful write keeps a backup next to the file.
    #[test]
    fn the_backup_is_kept() {
        let (dir, path) = temp_config("key-backup");
        std::fs::write(&path, SAMPLE).expect("write the sample");

        write_key(&path, "balloons", "max", "9").expect("the write succeeds");

        let backup = dir.join("config.toml.bak");
        assert!(backup.exists(), "the write keeps a backup");
        let old = std::fs::read_to_string(&backup).expect("read the backup");
        assert_eq!(old, SAMPLE, "the backup is the old file: {old}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A config whose last table is followed by an array of tables.
    const WITH_ARRAY: &str = "loop_seconds = 12.0\n\n[audio]\ntrack = \"chirp\"\n\n[[personality]]\nname = \"ducky\"\n\n[[personality]]\nname = \"other\"\n";

    /// A key appended to a table that is followed by an array-of-tables
    /// block lands **in the table**, not in the block.
    ///
    /// WHY: the section's range used to end only at the next `[table]`
    /// header, so it ran to the end of the file and swallowed the
    /// `[[personality]]` block — and the key landed inside it (audit
    /// 2026-10-21 §F1).
    #[test]
    fn a_key_appended_to_a_table_does_not_land_in_the_array_block() {
        let (dir, path) = temp_config("key-append-array");
        std::fs::write(&path, WITH_ARRAY).expect("write the sample");

        write_key(&path, "audio", "strip_steps", "263").expect("the write succeeds");

        let after = std::fs::read_to_string(&path).expect("read back");
        let table = after.find("[audio]").expect("the table is there");
        let key = after.find("strip_steps = 263").expect("the key is there");
        let block = after.find("[[personality]]").expect("the block is there");
        assert!(
            table < key && key < block,
            "the key lands inside the table: {after}"
        );
        assert!(
            !after.contains("[[personality]]\nname = \"ducky\"\nstrip_steps"),
            "the array block stays clean: {after}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The section's range ends at the array header, so the table's own
    /// keys are the last ones in it.
    #[test]
    fn a_table_range_ends_at_the_array_header() {
        let range = find_section(WITH_ARRAY, "audio").expect("the table is there");
        let section = &WITH_ARRAY[range];
        assert!(
            !section.contains("[[personality]]"),
            "the range must not swallow the array: {section}"
        );
        assert!(section.contains("track = \"chirp\""));
    }

    /// A key that lives inside an array block is **not** a top-level
    /// inline key: the surgical write must not be refused because of it.
    #[test]
    fn a_key_inside_an_array_block_is_not_a_top_level_key() {
        let text = "[[personality]]\nname = \"ducky\"\n";
        assert!(
            !has_inline_key(text, "name"),
            "a key inside an array block is not top level"
        );
    }

    /// A top-level key that **also** appears inside an array block is
    /// still recognised as top level.
    #[test]
    fn a_top_level_key_that_also_appears_in_an_array_block_is_top_level() {
        let text = "name = \"top\"\n\n[[personality]]\nname = \"ducky\"\n";
        assert!(
            has_inline_key(text, "name"),
            "the top-level key is recognised"
        );
        let span = find_value_span(text, "", "name").expect("the top-level key");
        assert_eq!(
            text[span].trim(),
            "\"top\"",
            "the first one is the top-level one"
        );
    }

    /// A value inside an array block is attributed to the array, not to
    /// the table that came before it.
    #[test]
    fn a_value_inside_an_array_block_is_attributed_to_the_array() {
        let text = "[audio]\ntrack = \"chirp\"\n\n[[personality]]\nname = \"ducky\"\n";
        assert!(
            find_value_span(text, "audio", "name").is_none(),
            "the audio table does not own the array's key"
        );
        let span = find_value_span(text, "personality", "name").expect("the array's own key");
        assert_eq!(
            text[span].trim(),
            "\"ducky\"",
            "the value is the array's own"
        );
    }

    /// A trailing comment does not open a multi-line value.
    ///
    /// `#` ends the TOML of a line, so a bracket inside a comment used to
    /// be counted as an unclosed one: the value's span swallowed the
    /// lines after it and the splice landed somewhere else entirely
    /// (audit 2026-10-23 §F-047).
    #[test]
    fn a_comment_bracket_does_not_open_a_multiline_value() {
        let text = "max_cols = 260 # the old cap [\nloop_seconds = 12.0\n";
        let repaired = apply_repairs(
            text,
            std::slice::from_ref(
                &(Repair {
                    section: "",
                    key: "max_cols".to_string(),
                    value: "4096".to_string(),
                }),
            ),
        )
        .expect("the key is there");
        assert!(
            repaired.contains("max_cols = 4096 # the old cap ["),
            "the value is swapped, the comment survives: {repaired}"
        );
        assert!(
            repaired.contains("loop_seconds = 12.0"),
            "the next line is not swallowed: {repaired}"
        );
    }

    /// A closed array whose trailing comment carries an unbalanced bracket
    /// still splices correctly (audit 2026-10-23 §F-047).
    #[test]
    fn a_comment_after_an_array_does_not_confuse_the_depth() {
        let text = "cell_size = [12, 24] # unbalanced [\nloop_seconds = 12.0\n";
        let repaired = apply_repairs(
            text,
            std::slice::from_ref(
                &(Repair {
                    section: "",
                    key: "cell_size".to_string(),
                    value: "[10, 20]".to_string(),
                }),
            ),
        )
        .expect("the key is there");
        assert!(
            repaired.contains("cell_size = [10, 20] # unbalanced ["),
            "the array is replaced, the comment survives: {repaired}"
        );
        assert!(
            repaired.contains("loop_seconds = 12.0"),
            "the next line is not swallowed: {repaired}"
        );
        let cfg: Config = toml::from_str(&repaired).expect("the result must parse");
        assert_eq!(cfg.cell_size, [10, 20]);
    }

    /// A new key appended to a CRLF document is written with the
    /// document's own line ending, not with a bare `\n` (audit
    /// 2026-10-23 §F-057).
    #[test]
    fn an_appended_key_honours_the_crlf_document() {
        let (dir, path) = temp_config("key-crlf");
        let sample = "# my ducks\r\n\r\n[balloons]\r\nmax = 5\r\n";
        std::fs::write(&path, sample).expect("write the sample");

        write_key(&path, "balloons", "min", "2").expect("the write succeeds");

        let after = std::fs::read_to_string(&path).expect("read back");
        assert!(
            after.contains("min = 2\r\n"),
            "the appended key carries the document's CRLF: {after:?}"
        );
        // A CRLF blank line ("\r\n\r\n") contains the substring "\n\r",
        // so "no \n\r" would fail on the user's own blank lines. The
        // mixed-ending check is: every LF in the file is half of a CRLF.
        let bare_lfs = after.matches('\n').count() - after.matches("\r\n").count();
        assert_eq!(bare_lfs, 0, "no bare LF endings: {after:?}");
        let cfg: Config = toml::from_str(&after).expect("the result must parse");
        assert_eq!(cfg.balloons.max, 5);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
