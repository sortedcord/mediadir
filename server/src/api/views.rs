//! Response DTOs and their assembly from domain objects (the contract's JSON shapes).

use crate::db::{Member, Membership, Relation, Repo};
use crate::display::{FieldRegistry, PresentContext, Presenter};
use crate::error::AppResult;
use crate::model::{
    AudioStream, Credit, DisplayBlock, FieldType, Item, ItemKind, JsonMap, MediaFile, Person, PersonRef, VideoStream,
};
use serde::Serialize;
use std::collections::HashMap;

#[derive(Serialize)]
pub struct ItemSummary {
    pub id: String,
    pub kind: ItemKind,
    pub library_id: String,
    pub parent_id: Option<String>,
    pub title: String,
    pub subtitle: Option<String>,
    pub year: Option<i64>,
    pub index: Option<i64>,
    pub parent_index: Option<i64>,
    pub index_end: Option<i64>,
    pub duration: Option<f64>,
    pub playable: bool,
    pub child_count: Option<i64>,
    pub aspect: &'static str,
    pub added_at: i64,
    pub card: Vec<DisplayBlock>,
    /// Raw values of ALL image-type fields of the item, regardless of placement (omitted when none).
    #[serde(skip_serializing_if = "JsonMap::is_empty")]
    pub images: JsonMap,
}

#[derive(Serialize)]
pub struct Breadcrumb {
    pub id: String,
    pub kind: ItemKind,
    pub title: String,
}

#[derive(Serialize)]
pub struct SubtitleDto {
    pub index: u32,
    pub codec: String,
    pub language: Option<String>,
    pub title: Option<String>,
    pub forced: bool,
    pub external: Option<String>,
}

#[derive(Serialize)]
pub struct MediaFileDto {
    pub id: String,
    /// The original file name (the stored blob's location is private).
    pub path: String,
    pub original_name: String,
    pub hash: String,
    /// `store` (a file MediaDir holds) or `external` (the store entry is a symlink to media kept elsewhere).
    pub location: String,
    /// `full` or `pending` (`hash` is then `fp:<fingerprint>` until the background hash completes).
    pub hash_state: String,
    pub added_at: i64,
    pub label: String,
    pub original: bool,
    pub container: String,
    pub mime: String,
    pub size: u64,
    pub duration: Option<f64>,
    pub bitrate: Option<u64>,
    pub video: Option<VideoStream>,
    pub audio: Vec<AudioStream>,
    pub subtitles: Vec<SubtitleDto>,
    pub stream_url: String,
    pub part: Option<u32>,
    pub edition: Option<String>,
}

#[derive(Serialize)]
pub struct CreditDto {
    pub id: String,
    pub role: String,
    pub character: Option<String>,
    pub order: i64,
    pub source: &'static str,
    pub person: PersonRef,
}

#[derive(Serialize)]
pub struct ItemDetail {
    #[serde(flatten)]
    pub summary: ItemSummary,
    pub overview: Option<String>,
    pub sort_title: Option<String>,
    pub genres: Vec<String>,
    pub locked: Vec<String>,
    pub breadcrumbs: Vec<Breadcrumb>,
    pub files: Vec<MediaFileDto>,
    pub credits: Vec<CreditDto>,
    pub children: Vec<ItemSummary>,
    pub extras: Vec<ItemSummary>,
    pub fields: JsonMap,
    pub display: Vec<DisplayBlock>,
    pub relations: Vec<ItemRelation>,
}

#[derive(Serialize)]
pub struct RelationSummary {
    pub id: String,
    #[serde(rename = "type")]
    pub rtype: String,
    pub name: String,
    pub member_count: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A relation as seen from one of its members.
#[derive(Serialize)]
pub struct ItemRelation {
    #[serde(flatten)]
    pub relation: RelationSummary,
    pub ord: i64,
    pub role: Option<String>,
}

#[derive(Serialize)]
pub struct RelationMember {
    pub item_id: String,
    pub ord: i64,
    pub role: Option<String>,
    pub item: ItemSummary,
}

#[derive(Serialize)]
pub struct RelationDetail {
    #[serde(flatten)]
    pub summary: RelationSummary,
    pub members: Vec<RelationMember>,
}

#[derive(Serialize)]
pub struct PersonSummary {
    pub id: String,
    pub name: String,
    pub sort_name: Option<String>,
    pub credit_count: i64,
    /// Raw values of all image-type fields of the person (omitted when none).
    #[serde(skip_serializing_if = "JsonMap::is_empty")]
    pub images: JsonMap,
}

#[derive(Serialize)]
pub struct PersonCredit {
    pub id: String,
    pub role: String,
    pub character: Option<String>,
    pub item: ItemSummary,
}

#[derive(Serialize)]
pub struct PersonDetail {
    #[serde(flatten)]
    pub summary: PersonSummary,
    pub bio: Option<String>,
    pub birth_date: Option<String>,
    pub fields: JsonMap,
    pub display: Vec<DisplayBlock>,
    pub credits: Vec<PersonCredit>,
}

#[derive(Serialize)]
pub struct QueueEntry {
    /// Always null in v3 (no stacking); kept for the v2 shape.
    pub part: Option<u32>,
    pub item: ItemSummary,
    pub files: Vec<MediaFileDto>,
}

#[derive(Serialize)]
pub struct Playback {
    pub start_index: usize,
    pub queue: Vec<QueueEntry>,
}



pub fn media_file_dto(f: &MediaFile) -> MediaFileDto {
    let subtitles = f
        .subtitles
        .iter()
        .map(|s| SubtitleDto {
            index: s.index,
            codec: s.codec.clone(),
            language: s.language.clone(),
            title: s.title.clone(),
            forced: s.forced,
            external: None,
        })
        .collect();
    MediaFileDto {
        id: f.id.clone(),
        path: f.original_name.clone(),
        original_name: f.original_name.clone(),
        hash: f.hash.clone(),
        location: f.location.clone(),
        hash_state: f.hash_state.clone(),
        added_at: f.added_at,
        label: f.label.clone(),
        original: true,
        container: f.container.clone(),
        mime: f.mime.clone(),
        size: f.size,
        duration: f.duration,
        bitrate: f.bitrate,
        video: f.video.clone(),
        audio: f.audio.clone(),
        subtitles,
        stream_url: format!("/stream/{}", f.id),
        part: None,
        edition: None,
    }
}

/// Assembles contract objects; holds the field registry for one request.
pub struct Views<'r, 'c> {
    pub repo: &'r Repo<'c>,
    pub registry: FieldRegistry,
}

impl<'r, 'c> Views<'r, 'c> {
    pub fn new(repo: &'r Repo<'c>) -> AppResult<Self> {
        Ok(Views { repo, registry: FieldRegistry::load(repo)? })
    }

    /// The raw values of every image-type field in `values`, whatever its placement.
    fn image_values(&self, values: &JsonMap) -> JsonMap {
        values
            .iter()
            .filter(|(k, v)| {
                !v.is_null() && self.registry.get(k).map_or(false, |d| d.field_type == FieldType::Image)
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    fn person_ref(&self, p: &Person) -> PersonRef {
        PersonRef { id: p.id.clone(), name: p.name.clone(), images: self.image_values(&p.fields) }
    }

    fn people_for(&self, values: &[&JsonMap]) -> AppResult<HashMap<String, PersonRef>> {
        let presenter = Presenter::new(&self.registry);
        let ids: Vec<String> = values.iter().flat_map(|m| presenter.referenced_people(m.iter())).collect();
        Ok(self.repo.people_by_ids(&ids)?.iter().map(|p| (p.id.clone(), self.person_ref(p))).collect())
    }

    fn merged_fields(item: &Item) -> JsonMap {
        let mut m = item.extra.clone();
        for (k, v) in &item.fields {
            m.insert(k.clone(), v.clone());
        }
        m
    }

    fn summary_with(&self, item: &Item, files: &[MediaFile], parent: Option<&Item>) -> AppResult<ItemSummary> {
        let merged = Self::merged_fields(item);
        let people = self.people_for(&[&merged])?;
        let ctx = PresentContext { item, files, parent, people: &people };
        let card = Presenter::new(&self.registry).card(&ctx);
        let playable = item.kind.playable();
        let duration = if playable {
            item.duration.or_else(|| files.first().and_then(|f| f.duration))
        } else {
            None
        };
        Ok(ItemSummary {
            id: item.id.clone(),
            kind: item.kind,
            library_id: item.library_id(),
            parent_id: item.parent_id.clone(),
            title: item.title.clone(),
            subtitle: item.subtitle.clone(),
            year: item.year,
            index: item.index,
            parent_index: item.parent_index,
            index_end: item.index_end,
            duration,
            playable,
            child_count: if playable { None } else { Some(self.repo.child_count(&item.id)?) },
            aspect: item.kind.aspect(),
            added_at: item.added_at,
            card,
            images: self.image_values(&merged),
        })
    }

    fn parent_of(&self, item: &Item) -> AppResult<Option<Item>> {
        match (&item.parent_id, item.kind) {
            (Some(pid), ItemKind::Track) => self.repo.find_item(pid),
            _ => Ok(None),
        }
    }

    pub fn summary(&self, item: &Item) -> AppResult<ItemSummary> {
        let files = if item.kind.playable() { self.repo.files_for_item(&item.id)? } else { vec![] };
        let parent = self.parent_of(item)?;
        self.summary_with(item, &files, parent.as_ref())
    }

    pub fn summaries(&self, items: &[Item]) -> AppResult<Vec<ItemSummary>> {
        items.iter().map(|i| self.summary(i)).collect()
    }

    pub fn credits(&self, credits: &[Credit]) -> AppResult<Vec<CreditDto>> {
        let ids: Vec<String> = credits.iter().map(|c| c.person_id.clone()).collect();
        let people: HashMap<String, Person> =
            self.repo.people_by_ids(&ids)?.into_iter().map(|p| (p.id.clone(), p)).collect();
        Ok(credits
            .iter()
            .filter_map(|c| {
                let p = people.get(&c.person_id)?;
                Some(CreditDto {
                    id: c.id.clone(),
                    role: c.role.as_str().to_string(),
                    character: c.character.clone(),
                    order: c.order,
                    source: c.source.as_str(),
                    person: self.person_ref(p),
                })
            })
            .collect())
    }

    pub fn detail(&self, item: &Item) -> AppResult<ItemDetail> {
        let files = self.repo.files_for_item(&item.id)?;
        let parent = self.parent_of(item)?;
        let summary = self.summary_with(item, &files, parent.as_ref())?;
        let merged = Self::merged_fields(item);
        let people = self.people_for(&[&merged])?;
        let ctx = PresentContext { item, files: &files, parent: parent.as_ref(), people: &people };
        let display = Presenter::new(&self.registry).detail(&ctx);

        let mut breadcrumbs = vec![];
        let mut cur = item.parent_id.clone();
        while let Some(pid) = cur {
            let Some(p) = self.repo.find_item(&pid)? else { break };
            cur = p.parent_id.clone();
            breadcrumbs.push(Breadcrumb { id: p.id, kind: p.kind, title: p.title });
            if breadcrumbs.len() > 5 {
                break;
            }
        }
        breadcrumbs.reverse();

        let mut children = self.repo.children(&item.id)?;
        if item.kind == ItemKind::Artist {
            children.sort_by_key(|c| (c.year.unwrap_or(i64::MAX), c.title.to_lowercase()));
        }
        Ok(ItemDetail {
            summary,
            overview: item.overview.clone(),
            sort_title: item.sort_title.clone(),
            genres: item.genres.clone(),
            locked: item.locked.clone(),
            breadcrumbs,
            files: files.iter().map(media_file_dto).collect(),
            credits: self.credits(&self.repo.credits_for_item(&item.id)?)?,
            children: self.summaries(&children)?,
            extras: vec![],
            fields: merged,
            display,
            relations: self.item_relations(&item.id)?,
        })
    }

    pub fn relation_summary(r: &Relation) -> RelationSummary {
        RelationSummary {
            id: r.id.clone(),
            rtype: r.rtype.clone(),
            name: r.name.clone(),
            member_count: r.member_count,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }

    pub fn item_relations(&self, item_id: &str) -> AppResult<Vec<ItemRelation>> {
        Ok(self
            .repo
            .relations_for_item(item_id)?
            .iter()
            .map(|Membership { relation, ord, role }| ItemRelation {
                relation: Self::relation_summary(relation),
                ord: *ord,
                role: role.clone(),
            })
            .collect())
    }

    pub fn relation_detail(&self, r: &Relation) -> AppResult<RelationDetail> {
        let mut members = vec![];
        for (Member { item_id, ord, role }, item) in self.repo.relation_members(&r.id)? {
            members.push(RelationMember { item_id, ord, role, item: self.summary(&item)? });
        }
        Ok(RelationDetail { summary: Self::relation_summary(r), members })
    }

    pub fn person_summary(&self, p: &Person) -> AppResult<PersonSummary> {
        Ok(PersonSummary {
            id: p.id.clone(),
            name: p.name.clone(),
            sort_name: p.sort_name.clone(),
            credit_count: self.repo.credit_count(&p.id)?,
            images: self.image_values(&p.fields),
        })
    }

    pub fn person_detail(&self, p: &Person) -> AppResult<PersonDetail> {
        let people = self.people_for(&[&p.fields])?;
        let display = Presenter::new(&self.registry).person(p, &people);
        let mut credits = vec![];
        for c in self.repo.credits_for_person(&p.id)? {
            if let Some(item) = self.repo.find_item(&c.item_id)? {
                credits.push(PersonCredit {
                    id: c.id,
                    role: c.role.as_str().to_string(),
                    character: c.character,
                    item: self.summary(&item)?,
                });
            }
        }
        Ok(PersonDetail {
            summary: self.person_summary(p)?,
            bio: p.bio.clone(),
            birth_date: p.birth_date.clone(),
            fields: p.fields.clone(),
            display,
            credits,
        })
    }

    pub fn playback(&self, item: &Item) -> AppResult<Playback> {
        let queue_items: Vec<Item> = if item.kind.playable() {
            match &item.parent_id {
                Some(pid) => {
                    let siblings = self.repo.children(pid)?;
                    let pos = siblings.iter().position(|s| s.id == item.id).unwrap_or(0);
                    siblings.into_iter().skip(pos).filter(|s| s.kind.playable()).collect()
                }
                None => vec![item.clone()],
            }
        } else {
            self.repo.playable_descendants(&item.id)?
        };
        let mut queue = vec![];
        for qi in &queue_items {
            let files = self.repo.files_for_item(&qi.id)?;
            let parent = self.parent_of(qi)?;
            queue.push(QueueEntry {
                part: None,
                item: self.summary_with(qi, &files, parent.as_ref())?,
                files: files.iter().map(media_file_dto).collect(),
            });
        }
        Ok(Playback { start_index: 0, queue })
    }
}
