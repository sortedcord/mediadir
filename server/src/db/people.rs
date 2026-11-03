use super::{json_col, now, to_json, Repo};
use crate::error::{AppError, AppResult};
use crate::model::{credit_id, person_id, Credit, CreditSource, JsonMap, Person, Role};
use rusqlite::{params, params_from_iter, OptionalExtension, Row};

const COLS: &str = "id, name, sort_name, bio, birth_date, image, fields";

fn row_to_person(r: &Row) -> rusqlite::Result<Person> {
    Ok(Person {
        id: r.get(0)?,
        name: r.get(1)?,
        sort_name: r.get(2)?,
        bio: r.get(3)?,
        birth_date: r.get(4)?,
        image: r.get(5)?,
        fields: json_col(r.get(6)?),
    })
}

fn row_to_credit(r: &Row) -> rusqlite::Result<Credit> {
    let role: String = r.get(3)?;
    let source: String = r.get(6)?;
    Ok(Credit {
        id: r.get(0)?,
        item_id: r.get(1)?,
        person_id: r.get(2)?,
        role: Role::new(&role),
        character: r.get(4)?,
        order: r.get(5)?,
        source: CreditSource::parse(&source),
    })
}

/// "Nora Fatehi" -> "Fatehi, Nora"; single names unchanged.
pub fn default_sort_name(name: &str) -> String {
    let parts: Vec<&str> = name.split_whitespace().collect();
    match parts.as_slice() {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{last}, {}", rest.join(" ")),
    }
}

/// A credit to create from scanned metadata.
#[derive(Debug, Clone)]
pub struct CreditDraft {
    pub name: String,
    pub role: Role,
    pub character: Option<String>,
    pub order: i64,
    /// Local image path discovered for the person (e.g. NFO `<thumb>`).
    pub image: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct PeopleQuery {
    pub q: Option<String>,
    pub role: Option<String>,
    pub limit: i64,
    pub offset: i64,
}

impl Repo<'_> {
    pub fn get_person(&self, id: &str) -> AppResult<Person> {
        self.find_person(id)?.ok_or_else(|| AppError::not_found("person"))
    }

    pub fn find_person(&self, id: &str) -> AppResult<Option<Person>> {
        Ok(self
            .conn
            .query_row(&format!("SELECT {COLS} FROM people WHERE id = ?1"), [id], row_to_person)
            .optional()?)
    }

    pub fn people_by_ids(&self, ids: &[String]) -> AppResult<Vec<Person>> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let ph = (1..=ids.len()).map(|i| format!("?{i}")).collect::<Vec<_>>().join(",");
        let mut st = self.conn.prepare(&format!("SELECT {COLS} FROM people WHERE id IN ({ph})"))?;
        let rows = st.query_map(params_from_iter(ids.iter()), row_to_person)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn credit_count(&self, person_id: &str) -> AppResult<i64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM credits WHERE person_id = ?1", [person_id], |r| r.get(0))?)
    }

    pub fn list_people(&self, q: &PeopleQuery) -> AppResult<(i64, Vec<Person>)> {
        let mut wh = vec![];
        let mut args: Vec<String> = vec![];
        if let Some(s) = q.q.as_ref().filter(|s| !s.trim().is_empty()) {
            args.push(format!("%{}%", s.trim().replace('%', "\\%").replace('_', "\\_")));
            wh.push(format!("(name LIKE ?{0} ESCAPE '\\' OR sort_name LIKE ?{0} ESCAPE '\\')", args.len()));
        }
        if let Some(r) = q.role.as_ref().filter(|s| !s.trim().is_empty()) {
            args.push(Role::new(r).as_str().to_string());
            wh.push(format!("id IN (SELECT person_id FROM credits WHERE role = ?{})", args.len()));
        }
        let where_sql = if wh.is_empty() { String::new() } else { format!("WHERE {}", wh.join(" AND ")) };
        let total = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM people {where_sql}"),
            params_from_iter(args.iter()),
            |r| r.get(0),
        )?;
        let mut st = self.conn.prepare(&format!(
            "SELECT {COLS} FROM people {where_sql} ORDER BY COALESCE(sort_name, name) COLLATE NOCASE, id LIMIT {} OFFSET {}",
            q.limit.clamp(1, 1000),
            q.offset.max(0)
        ))?;
        let rows = st.query_map(params_from_iter(args.iter()), row_to_person)?.collect::<Result<Vec<_>, _>>()?;
        Ok((total, rows))
    }

    pub fn search_people(&self, q: &str, limit: i64) -> AppResult<Vec<Person>> {
        Ok(self.list_people(&PeopleQuery { q: Some(q.into()), role: None, limit, offset: 0 })?.1)
    }

    pub fn insert_person(&self, p: &Person) -> AppResult<()> {
        if self.find_person(&p.id)?.is_some() {
            return Err(AppError::Conflict(format!("person '{}' already exists", p.name)));
        }
        self.conn.execute(
            &format!("INSERT INTO people ({COLS}, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)"),
            params![p.id, p.name, p.sort_name, p.bio, p.birth_date, p.image, to_json(&p.fields), now()],
        )?;
        Ok(())
    }

    pub fn update_person(&self, p: &Person) -> AppResult<()> {
        self.conn.execute(
            "UPDATE people SET name=?2, sort_name=?3, bio=?4, birth_date=?5, image=?6, fields=?7 WHERE id=?1",
            params![p.id, p.name, p.sort_name, p.bio, p.birth_date, p.image, to_json(&p.fields)],
        )?;
        Ok(())
    }

    pub fn delete_person(&self, id: &str) -> AppResult<()> {
        if self.conn.execute("DELETE FROM people WHERE id = ?1", [id])? == 0 {
            return Err(AppError::not_found("person"));
        }
        Ok(())
    }

    /// Find a person by (normalized) name or create it. Returns its id.
    pub fn ensure_person(&self, name: &str, image: Option<&str>) -> AppResult<String> {
        let id = person_id(name);
        match self.find_person(&id)? {
            Some(p) => {
                if p.image.is_none() && image.is_some() {
                    self.set_person_image(&id, image)?;
                }
            }
            None => {
                let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
                self.conn.execute(
                    "INSERT INTO people (id, name, sort_name, image, fields, created_at) VALUES (?1,?2,?3,?4,'{}',?5)",
                    params![id, name, default_sort_name(&name), image, now()],
                )?;
            }
        }
        Ok(id)
    }

    pub fn set_person_image(&self, id: &str, image: Option<&str>) -> AppResult<()> {
        self.conn.execute("UPDATE people SET image = ?2 WHERE id = ?1", params![id, image])?;
        Ok(())
    }

    pub fn set_person_fields(&self, id: &str, fields: &JsonMap) -> AppResult<()> {
        self.conn.execute("UPDATE people SET fields = ?2 WHERE id = ?1", params![id, to_json(fields)])?;
        Ok(())
    }

    // ---- credits ----

    pub fn credits_for_item(&self, item_id: &str) -> AppResult<Vec<Credit>> {
        let mut st = self.conn.prepare(
            "SELECT id, item_id, person_id, role, character, ord, source FROM credits WHERE item_id = ?1 ORDER BY ord, role, id",
        )?;
        let rows = st.query_map([item_id], row_to_credit)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn credits_for_person(&self, person_id: &str) -> AppResult<Vec<Credit>> {
        let mut st = self.conn.prepare(
            "SELECT c.id, c.item_id, c.person_id, c.role, c.character, c.ord, c.source FROM credits c
             JOIN items i ON i.id = c.item_id WHERE c.person_id = ?1
             ORDER BY COALESCE(i.year, 0) DESC, i.title COLLATE NOCASE",
        )?;
        let rows = st.query_map([person_id], row_to_credit)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn add_manual_credit(
        &self,
        item_id: &str,
        person_id: &str,
        role: &Role,
        character: Option<&str>,
        order: Option<i64>,
    ) -> AppResult<String> {
        let id = credit_id(item_id, person_id, role.as_str(), true);
        let order = match order {
            Some(o) => o,
            None => self.conn.query_row(
                "SELECT COALESCE(MAX(ord) + 1, 0) FROM credits WHERE item_id = ?1",
                [item_id],
                |r| r.get(0),
            )?,
        };
        self.conn.execute(
            "INSERT OR REPLACE INTO credits (id, item_id, person_id, role, character, ord, source)
             VALUES (?1,?2,?3,?4,?5,?6,'manual')",
            params![id, item_id, person_id, role.as_str(), character, order],
        )?;
        // a manual credit supersedes an identical scanned one
        self.conn.execute(
            "DELETE FROM credits WHERE item_id=?1 AND person_id=?2 AND role=?3 AND source='scan'",
            params![item_id, person_id, role.as_str()],
        )?;
        Ok(id)
    }

    pub fn delete_credit(&self, item_id: &str, credit_id: &str) -> AppResult<()> {
        let n = self.conn.execute("DELETE FROM credits WHERE id = ?1 AND item_id = ?2", params![credit_id, item_id])?;
        if n == 0 {
            return Err(AppError::not_found("credit"));
        }
        Ok(())
    }

    /// Replace the scanner-produced credits of an item. Manual credits are untouched, and a
    /// scanned credit duplicating a manual one (same person + role) is skipped.
    pub fn replace_scan_credits(&self, item_id: &str, drafts: &[CreditDraft]) -> AppResult<()> {
        self.conn.execute("DELETE FROM credits WHERE item_id = ?1 AND source = 'scan'", [item_id])?;
        for d in drafts {
            if d.name.trim().is_empty() {
                continue;
            }
            let pid = self.ensure_person(&d.name, d.image.as_deref())?;
            let manual: i64 = self.conn.query_row(
                "SELECT COUNT(*) FROM credits WHERE item_id=?1 AND person_id=?2 AND role=?3 AND source='manual'",
                params![item_id, pid, d.role.as_str()],
                |r| r.get(0),
            )?;
            if manual > 0 {
                continue;
            }
            let id = credit_id(item_id, &pid, d.role.as_str(), false);
            self.conn.execute(
                "INSERT OR IGNORE INTO credits (id, item_id, person_id, role, character, ord, source)
                 VALUES (?1,?2,?3,?4,?5,?6,'scan')",
                params![id, item_id, pid, d.role.as_str(), d.character, d.order],
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::default_sort_name;
    #[test]
    fn sort_names() {
        assert_eq!(default_sort_name("Nora Fatehi"), "Fatehi, Nora");
        assert_eq!(default_sort_name("Ikka"), "Ikka");
        assert_eq!(default_sort_name("John  Q Public"), "Public, John Q");
    }
}
