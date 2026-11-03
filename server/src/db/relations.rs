use super::{json_col, now, to_json, Repo};
use crate::error::{AppError, AppResult};
use crate::model::{fnv1a, Item, JsonMap};
use rusqlite::{params, params_from_iter, OptionalExtension, Row};

const COLS: &str =
    "r.id, r.type, r.name, r.props, r.created_at, r.updated_at, (SELECT COUNT(*) FROM relation_members m WHERE m.relation_id = r.id)";

#[derive(Debug, Clone)]
pub struct Relation {
    pub id: String,
    pub rtype: String,
    pub name: String,
    pub props: JsonMap,
    pub created_at: i64,
    pub updated_at: i64,
    pub member_count: i64,
}

#[derive(Debug, Clone)]
pub struct Member {
    pub item_id: String,
    pub ord: i64,
    pub role: Option<String>,
}

/// A relation an item belongs to, with that item's position in it.
#[derive(Debug, Clone)]
pub struct Membership {
    pub relation: Relation,
    pub ord: i64,
    pub role: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct RelationQuery {
    pub rtype: Option<String>,
    pub q: Option<String>,
    pub limit: i64,
    pub offset: i64,
}

fn row_to_relation(r: &Row) -> rusqlite::Result<Relation> {
    Ok(Relation {
        id: r.get(0)?,
        rtype: r.get(1)?,
        name: r.get(2)?,
        props: json_col(r.get(3)?),
        created_at: r.get(4)?,
        updated_at: r.get(5)?,
        member_count: r.get(6)?,
    })
}

/// Trim, lowercase, spaces -> underscores; must be a non-empty `[a-z0-9_]` string.
pub fn normalize_type(s: &str) -> AppResult<String> {
    let t = s.split_whitespace().collect::<Vec<_>>().join("_").to_lowercase();
    if t.is_empty() {
        return Err(AppError::bad("type is required"));
    }
    if !t.chars().all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_')) {
        return Err(AppError::bad("type may only contain letters, digits, spaces and underscores"));
    }
    Ok(t)
}

pub fn new_relation_id() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    fnv1a(&format!("relation\u{0}{nanos}\u{0}{n}\u{0}{}", std::process::id()))
}

impl Repo<'_> {
    pub fn get_relation(&self, id: &str) -> AppResult<Relation> {
        Ok(self
            .conn
            .query_row(&format!("SELECT {COLS} FROM relations r WHERE r.id = ?1"), [id], row_to_relation)
            .optional()?
            .ok_or_else(|| AppError::not_found("relation"))?)
    }

    pub fn list_relations(&self, q: &RelationQuery) -> AppResult<(i64, Vec<Relation>)> {
        let mut wh = vec![];
        let mut args: Vec<String> = vec![];
        if let Some(t) = q.rtype.as_ref().filter(|s| !s.trim().is_empty()) {
            args.push(normalize_type(t)?);
            wh.push(format!("r.type = ?{}", args.len()));
        }
        if let Some(s) = q.q.as_ref().filter(|s| !s.trim().is_empty()) {
            args.push(format!("%{}%", s.trim().replace('%', "\\%").replace('_', "\\_")));
            wh.push(format!("r.name LIKE ?{} ESCAPE '\\'", args.len()));
        }
        let where_sql = if wh.is_empty() { String::new() } else { format!("WHERE {}", wh.join(" AND ")) };
        let total = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM relations r {where_sql}"),
            params_from_iter(args.iter()),
            |r| r.get(0),
        )?;
        let mut st = self.conn.prepare(&format!(
            "SELECT {COLS} FROM relations r {where_sql} ORDER BY r.name COLLATE NOCASE, r.created_at, r.id LIMIT {} OFFSET {}",
            q.limit.clamp(1, 1000),
            q.offset.max(0)
        ))?;
        let rows = st.query_map(params_from_iter(args.iter()), row_to_relation)?.collect::<Result<Vec<_>, _>>()?;
        Ok((total, rows))
    }

    pub fn relation_types(&self) -> AppResult<Vec<(String, i64)>> {
        let mut st = self.conn.prepare("SELECT type, COUNT(*) FROM relations GROUP BY type ORDER BY type")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Insert the relation row only; members are added with [`Repo::set_relation_members`].
    pub fn insert_relation(&self, id: &str, rtype: &str, name: &str, props: &JsonMap) -> AppResult<()> {
        let t = now();
        self.conn.execute(
            "INSERT INTO relations (id, type, name, props, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?5)",
            params![id, rtype, name, to_json(props), t],
        )?;
        Ok(())
    }

    pub fn update_relation(&self, r: &Relation) -> AppResult<()> {
        self.conn.execute(
            "UPDATE relations SET type=?2, name=?3, props=?4, updated_at=?5 WHERE id=?1",
            params![r.id, r.rtype, r.name, to_json(&r.props), now()],
        )?;
        Ok(())
    }

    pub fn delete_relation(&self, id: &str) -> AppResult<()> {
        if self.conn.execute("DELETE FROM relations WHERE id = ?1", [id])? == 0 {
            return Err(AppError::not_found("relation"));
        }
        self.conn.execute("DELETE FROM relation_members WHERE relation_id = ?1", [id])?;
        Ok(())
    }

    pub fn relation_exists(&self, id: &str) -> AppResult<bool> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM relations WHERE id = ?1", [id], |r| r.get::<_, i64>(0))? > 0)
    }

    /// Members in `ord`, then title order, with their items.
    pub fn relation_members(&self, id: &str) -> AppResult<Vec<(Member, Item)>> {
        let mut st = self.conn.prepare(
            "SELECT m.item_id, m.ord, m.role FROM relation_members m JOIN items i ON i.id = m.item_id
             WHERE m.relation_id = ?1 ORDER BY m.ord, i.title COLLATE NOCASE, m.item_id",
        )?;
        let members = st
            .query_map([id], |r| Ok(Member { item_id: r.get(0)?, ord: r.get(1)?, role: r.get(2)? }))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut out = vec![];
        for m in members {
            let item = self.get_item(&m.item_id)?;
            out.push((m, item));
        }
        Ok(out)
    }

    /// Replace the member list: upsert the given members, drop the rest. Empty list removes the
    /// relation (via the cleanup trigger). Items must exist.
    pub fn set_relation_members(&self, id: &str, members: &[Member]) -> AppResult<()> {
        for m in members {
            self.get_item(&m.item_id)?;
            self.conn.execute(
                "INSERT INTO relation_members (relation_id, item_id, ord, role) VALUES (?1,?2,?3,?4)
                 ON CONFLICT(relation_id, item_id) DO UPDATE SET ord = excluded.ord, role = excluded.role",
                params![id, m.item_id, m.ord, m.role],
            )?;
        }
        let keep: Vec<&String> = members.iter().map(|m| &m.item_id).collect();
        let ph = (2..2 + keep.len()).map(|i| format!("?{i}")).collect::<Vec<_>>().join(",");
        let sql = if keep.is_empty() {
            "DELETE FROM relation_members WHERE relation_id = ?1".to_string()
        } else {
            format!("DELETE FROM relation_members WHERE relation_id = ?1 AND item_id NOT IN ({ph})")
        };
        let args: Vec<&str> = std::iter::once(id).chain(keep.iter().map(|s| s.as_str())).collect();
        self.conn.execute(&sql, params_from_iter(args))?;
        self.touch_relation(id)
    }

    /// Add or update one member. A missing `ord` keeps the current one (or appends); a missing
    /// `role` keeps the current one.
    pub fn upsert_relation_member(&self, id: &str, item_id: &str, ord: Option<i64>, role: Option<Option<String>>) -> AppResult<()> {
        self.get_item(item_id)?;
        let cur: Option<(i64, Option<String>)> = self
            .conn
            .query_row(
                "SELECT ord, role FROM relation_members WHERE relation_id = ?1 AND item_id = ?2",
                params![id, item_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let ord = match (ord, &cur) {
            (Some(o), _) => o,
            (None, Some((o, _))) => *o,
            (None, None) => self.conn.query_row(
                "SELECT COALESCE(MAX(ord) + 1, 0) FROM relation_members WHERE relation_id = ?1",
                [id],
                |r| r.get(0),
            )?,
        };
        let role = match (role, cur) {
            (Some(r), _) => r,
            (None, Some((_, r))) => r,
            (None, None) => None,
        };
        self.conn.execute(
            "INSERT INTO relation_members (relation_id, item_id, ord, role) VALUES (?1,?2,?3,?4)
             ON CONFLICT(relation_id, item_id) DO UPDATE SET ord = excluded.ord, role = excluded.role",
            params![id, item_id, ord, role],
        )?;
        self.touch_relation(id)
    }

    pub fn remove_relation_member(&self, id: &str, item_id: &str) -> AppResult<()> {
        let n = self.conn.execute(
            "DELETE FROM relation_members WHERE relation_id = ?1 AND item_id = ?2",
            params![id, item_id],
        )?;
        if n == 0 {
            return Err(AppError::not_found("member"));
        }
        if self.relation_exists(id)? {
            self.touch_relation(id)?;
        }
        Ok(())
    }

    fn touch_relation(&self, id: &str) -> AppResult<()> {
        self.conn.execute("UPDATE relations SET updated_at = ?2 WHERE id = ?1", params![id, now()])?;
        Ok(())
    }

    pub fn relations_for_item(&self, item_id: &str) -> AppResult<Vec<Membership>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {COLS}, m.ord, m.role FROM relation_members m JOIN relations r ON r.id = m.relation_id
             WHERE m.item_id = ?1 ORDER BY r.type, r.name COLLATE NOCASE, r.id"
        ))?;
        let rows = st
            .query_map([item_id], |r| Ok(Membership { relation: row_to_relation(r)?, ord: r.get(7)?, role: r.get(8)? }))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_type;
    #[test]
    fn types_normalize() {
        assert_eq!(normalize_type("  Soundtrack Of ").unwrap(), "soundtrack_of");
        assert!(normalize_type("   ").is_err());
        assert!(normalize_type("a-b").is_err());
    }
}
