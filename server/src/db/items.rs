use super::{json_col, now_ms, to_json, Repo};
use crate::error::{AppError, AppResult};
use crate::model::{Item, ItemKind, MediaFile};
use rusqlite::{params, params_from_iter, OptionalExtension, Row};
use std::collections::HashMap;

const COLS: &str = "id, kind, parent_id, title, sort_title, subtitle, year, idx, parent_index, index_end, \
    overview, genres, duration, added_at, updated_at, locked, thumb, extra, fields, group_key";

fn row_to_item(r: &Row) -> rusqlite::Result<Item> {
    let kind: String = r.get(1)?;
    Ok(Item {
        id: r.get(0)?,
        kind: ItemKind::parse(&kind).unwrap_or(ItemKind::Video),
        parent_id: r.get(2)?,
        title: r.get(3)?,
        sort_title: r.get(4)?,
        subtitle: r.get(5)?,
        year: r.get(6)?,
        index: r.get(7)?,
        parent_index: r.get(8)?,
        index_end: r.get(9)?,
        overview: r.get(10)?,
        genres: json_col(r.get(11)?),
        duration: r.get(12)?,
        added_at: r.get(13)?,
        updated_at: r.get(14)?,
        locked: json_col(r.get(15)?),
        thumb: r.get(16)?,
        extra: json_col(r.get(17)?),
        fields: json_col(r.get(18)?),
        group_key: r.get(19)?,
    })
}

/// What a delete removed: the items (for thumbnails) and the files (for blobs).
#[derive(Debug, Default)]
pub struct Removed {
    pub items: Vec<Item>,
    pub files: Vec<MediaFile>,
}

/// Natural ordering for children: disc/season, number, year, then title.
const NATURAL_ORDER: &str = "COALESCE(parent_index, 0), COALESCE(idx, 1000000), COALESCE(year, 0), \
    COALESCE(sort_title, title) COLLATE NOCASE";

#[derive(Debug, Default, Clone)]
pub struct ItemQuery {
    pub library_id: Option<String>,
    pub kind: Option<String>,
    pub parent_id: Option<String>,
    pub person_id: Option<String>,
    pub q: Option<String>,
    pub sort: Option<String>,
    pub desc: Option<bool>,
    pub top: bool,
    pub limit: i64,
    pub offset: i64,
}

impl Repo<'_> {
    pub fn get_item(&self, id: &str) -> AppResult<Item> {
        self.find_item(id)?.ok_or_else(|| AppError::not_found("item"))
    }

    pub fn find_item(&self, id: &str) -> AppResult<Option<Item>> {
        Ok(self
            .conn
            .query_row(&format!("SELECT {COLS} FROM items WHERE id = ?1"), [id], row_to_item)
            .optional()?)
    }

    pub fn children(&self, parent_id: &str) -> AppResult<Vec<Item>> {
        let mut st =
            self.conn.prepare(&format!("SELECT {COLS} FROM items WHERE parent_id = ?1 ORDER BY {NATURAL_ORDER}"))?;
        let rows = st.query_map([parent_id], row_to_item)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn child_count(&self, parent_id: &str) -> AppResult<i64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM items WHERE parent_id = ?1", [parent_id], |r| r.get(0))?)
    }

    /// All playable descendants in natural order (depth first).
    pub fn playable_descendants(&self, id: &str) -> AppResult<Vec<Item>> {
        let mut out = vec![];
        for child in self.children(id)? {
            if child.kind.playable() {
                out.push(child);
            } else {
                let cid = child.id.clone();
                out.extend(self.playable_descendants(&cid)?);
            }
        }
        Ok(out)
    }

    pub fn query_items(&self, q: &ItemQuery) -> AppResult<(i64, Vec<Item>)> {
        let mut wh: Vec<String> = vec![];
        let mut args: Vec<String> = vec![];
        let mut push = |clause: &str, val: &str, wh: &mut Vec<String>| {
            args.push(val.to_string());
            wh.push(clause.replace('?', &format!("?{}", args.len())));
        };
        if let Some(v) = &q.library_id {
            push("library_id = ?", v, &mut wh);
        }
        if let Some(v) = &q.kind {
            push("kind = ?", v, &mut wh);
        }
        if let Some(v) = &q.parent_id {
            push("parent_id = ?", v, &mut wh);
        } else if q.top {
            wh.push("parent_id IS NULL".into());
        }
        if let Some(v) = &q.person_id {
            push("id IN (SELECT item_id FROM credits WHERE person_id = ?)", v, &mut wh);
        }
        if let Some(v) = q.q.as_ref().filter(|s| !s.trim().is_empty()) {
            let like = format!("%{}%", v.trim().replace('%', "\\%").replace('_', "\\_"));
            // both placeholders refer to the same numbered argument
            push("(title LIKE ? ESCAPE '\\' OR subtitle LIKE ? ESCAPE '\\')", &like, &mut wh);
        }
        let where_sql = if wh.is_empty() { String::new() } else { format!("WHERE {}", wh.join(" AND ")) };
        let sort = q.sort.as_deref().unwrap_or(if q.parent_id.is_some() { "index" } else { "title" });
        let desc = q.desc.unwrap_or(sort == "added");
        let dir = if desc { "DESC" } else { "ASC" };
        let order = match sort {
            "added" => format!("added_at {dir}, COALESCE(sort_title, title) COLLATE NOCASE"),
            "year" => format!("COALESCE(year, 0) {dir}, COALESCE(sort_title, title) COLLATE NOCASE"),
            "index" => format!(
                "COALESCE(parent_index, 0) {dir}, COALESCE(idx, 1000000) {dir}, COALESCE(year, 0) {dir}, \
                 COALESCE(sort_title, title) COLLATE NOCASE {dir}"
            ),
            _ => format!("COALESCE(sort_title, title) COLLATE NOCASE {dir}"),
        };
        let total: i64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM items {where_sql}"),
            params_from_iter(args.iter()),
            |r| r.get(0),
        )?;
        let sql = format!(
            "SELECT {COLS} FROM items {where_sql} ORDER BY {order}, id LIMIT {} OFFSET {}",
            q.limit.clamp(1, 1000),
            q.offset.max(0)
        );
        let mut st = self.conn.prepare(&sql)?;
        let items = st.query_map(params_from_iter(args.iter()), row_to_item)?.collect::<Result<Vec<_>, _>>()?;
        Ok((total, items))
    }

    pub fn find_by_group_key(&self, key: &str) -> AppResult<Option<Item>> {
        Ok(self.conn.query_row(&format!("SELECT {COLS} FROM items WHERE group_key = ?1"), [key], row_to_item).optional()?)
    }

    /// Insert or update an item; `updated_at` is bumped.
    pub fn save_item(&self, i: &Item) -> AppResult<()> {
        self.conn.execute(
            &format!(
                "INSERT INTO items (library_id, {COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)
                 ON CONFLICT(id) DO UPDATE SET library_id=excluded.library_id, kind=excluded.kind,
                 parent_id=excluded.parent_id, title=excluded.title, sort_title=excluded.sort_title,
                 subtitle=excluded.subtitle, year=excluded.year, idx=excluded.idx, parent_index=excluded.parent_index,
                 index_end=excluded.index_end, overview=excluded.overview, genres=excluded.genres,
                 duration=excluded.duration, added_at=excluded.added_at, updated_at=excluded.updated_at,
                 locked=excluded.locked, thumb=excluded.thumb, extra=excluded.extra, fields=excluded.fields,
                 group_key=excluded.group_key"
            ),
            params![
                i.library_id(),
                i.id,
                i.kind.as_str(),
                i.parent_id,
                i.title,
                i.sort_title,
                i.subtitle,
                i.year,
                i.index,
                i.parent_index,
                i.index_end,
                i.overview,
                to_json(&i.genres),
                i.duration,
                i.added_at,
                now_ms().max(i.updated_at + 1),
                to_json(&i.locked),
                i.thumb,
                to_json(&i.extra),
                to_json(&i.fields),
                i.group_key,
            ],
        )?;
        Ok(())
    }

    pub fn all_items(&self) -> AppResult<HashMap<String, Item>> {
        let mut st = self.conn.prepare(&format!("SELECT {COLS} FROM items"))?;
        let rows = st.query_map([], row_to_item)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().map(|i| (i.id.clone(), i)).collect())
    }

    /// Top-level item count per (virtual) library id.
    pub fn top_level_counts(&self) -> AppResult<HashMap<String, i64>> {
        let mut st = self.conn.prepare("SELECT library_id, COUNT(*) FROM items WHERE parent_id IS NULL GROUP BY library_id")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().collect())
    }

    /// Delete an item with all descendants and their file rows, then containers left empty.
    /// Blobs are not touched here; the caller removes `Removed::files` from the store.
    pub fn remove_item_tree(&self, id: &str) -> AppResult<Removed> {
        let root = self.get_item(id)?;
        let mut out = Removed::default();
        let mut stack = vec![root.clone()];
        while let Some(item) = stack.pop() {
            stack.extend(self.children(&item.id)?);
            out.files.extend(self.files_for_item(&item.id)?);
            self.conn.execute("DELETE FROM media_files WHERE item_id = ?1", [&item.id])?;
            self.conn.execute("DELETE FROM items WHERE id = ?1", [&item.id])?;
            out.items.push(item);
        }
        self.prune_empty_ancestors(root.parent_id.as_deref(), &mut out)?;
        Ok(out)
    }

    /// Remove containers (season, show, artist...) that no longer have children.
    pub fn prune_empty_ancestors(&self, parent: Option<&str>, out: &mut Removed) -> AppResult<()> {
        let mut cur = parent.map(String::from);
        while let Some(pid) = cur {
            let Some(p) = self.find_item(&pid)? else { break };
            if p.kind.playable() || self.child_count(&p.id)? > 0 {
                break;
            }
            self.conn.execute("DELETE FROM items WHERE id = ?1", [&p.id])?;
            cur = p.parent_id.clone();
            out.items.push(p);
        }
        Ok(())
    }

    pub fn remove_field_everywhere(&self, key: &str) -> AppResult<()> {
        let path = format!("$.\"{key}\"");
        self.conn.execute(
            "UPDATE items SET fields = json_remove(fields, ?1), extra = json_remove(extra, ?1), updated_at = ?2",
            params![path, now_ms()],
        )?;
        self.conn.execute("UPDATE people SET fields = json_remove(fields, ?1)", [&path])?;
        Ok(())
    }

    pub fn search_items(&self, q: &str, limit: i64) -> AppResult<Vec<Item>> {
        let like = format!("%{}%", q.trim().replace('%', "\\%").replace('_', "\\_"));
        let mut st = self.conn.prepare(&format!(
            "SELECT {COLS} FROM items WHERE title LIKE ?1 ESCAPE '\\' OR subtitle LIKE ?1 ESCAPE '\\'
             ORDER BY CASE WHEN title LIKE ?2 ESCAPE '\\' THEN 0 ELSE 1 END, COALESCE(sort_title, title) COLLATE NOCASE
             LIMIT ?3"
        ))?;
        let prefix = format!("{}%", q.trim().replace('%', "\\%").replace('_', "\\_"));
        let rows = st.query_map(params![like, prefix, limit], row_to_item)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}
