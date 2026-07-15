use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use crate::threads::now_ms;

pub const EV_CLUES: &str = "clues:changed";

pub(crate) fn deserialize_vec_or_default<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<Vec<T>>::deserialize(deserializer).map(Option::unwrap_or_default)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClueCardVersion {
    pub id: String,
    pub title: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_thread_id: Option<String>,
    pub created_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClueCard {
    pub id: String,
    pub current_version_id: String,
    #[serde(default, deserialize_with = "deserialize_vec_or_default")]
    pub versions: Vec<ClueCardVersion>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl ClueCard {
    pub fn current_version(&self) -> Option<&ClueCardVersion> {
        self.versions
            .iter()
            .find(|version| version.id == self.current_version_id)
            .or_else(|| self.versions.last())
    }
}

/// 内部节点组：同组卡片共享同一组前置卡片，对用户只展示为平行后续线索。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClueNodeGroup {
    pub id: String,
    #[serde(default, deserialize_with = "deserialize_vec_or_default")]
    pub parent_card_ids: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_vec_or_default")]
    pub cards: Vec<ClueCard>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClueContextCard {
    pub card_id: String,
    pub version_id: String,
    pub title: String,
    pub content: String,
    #[serde(default, deserialize_with = "deserialize_vec_or_default")]
    pub parent_card_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClueContextSnapshot {
    pub root_card_id: String,
    #[serde(default, deserialize_with = "deserialize_vec_or_default")]
    pub cards: Vec<ClueContextCard>,
    pub rendered_context: String,
    pub created_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureClueResult {
    pub group: ClueNodeGroup,
    pub card: ClueCard,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClueFile {
    #[serde(default, deserialize_with = "deserialize_vec_or_default")]
    groups: Vec<ClueNodeGroup>,
}

pub struct ClueStore {
    path: PathBuf,
    pub groups: Vec<ClueNodeGroup>,
}

impl ClueStore {
    pub fn load(dir: &PathBuf) -> Self {
        let path = dir.join("clues.json");
        let groups = fs::read_to_string(&path)
            .ok()
            .and_then(|value| serde_json::from_str::<ClueFile>(&value).ok())
            .map(|file| file.groups)
            .unwrap_or_default();
        Self { path, groups }
    }

    pub fn list(&self) -> Vec<ClueNodeGroup> {
        let mut groups = self.groups.clone();
        groups.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        groups
    }

    pub fn replace(&mut self, groups: Vec<ClueNodeGroup>) -> Result<(), String> {
        self.groups = groups;
        self.save()
    }

    pub fn save(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let json = serde_json::to_string_pretty(&ClueFile {
            groups: self.groups.clone(),
        })
        .map_err(|error| error.to_string())?;
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, json).map_err(|error| error.to_string())?;
        if self.path.exists() {
            fs::remove_file(&self.path).map_err(|error| error.to_string())?;
        }
        fs::rename(tmp, &self.path).map_err(|error| error.to_string())
    }

    pub fn capture(
        &mut self,
        placement: &str,
        target_card_id: Option<&str>,
        title: String,
        content: String,
        source_thread_id: Option<String>,
    ) -> Result<CaptureClueResult, String> {
        let title = title.trim().to_string();
        let content = content.trim().to_string();
        if title.is_empty() {
            return Err("线索标题不能为空".into());
        }
        if content.is_empty() {
            return Err("线索内容不能为空".into());
        }
        let now = now_ms();

        let result = match placement {
            "update" => {
                let target = target_card_id.ok_or("请选择要更新的线索")?;
                let (group_index, card_index) =
                    self.card_location(target).ok_or("目标线索不存在")?;
                let version = new_version(title, content, source_thread_id, now);
                let group = &mut self.groups[group_index];
                let card = {
                    let card = &mut group.cards[card_index];
                    card.current_version_id = version.id.clone();
                    card.versions.push(version);
                    card.updated_at = now;
                    card.clone()
                };
                group.updated_at = now;
                CaptureClueResult {
                    group: group.clone(),
                    card,
                }
            }
            "parallel" => {
                let target = target_card_id.ok_or("请选择平行线索的位置")?;
                let (group_index, _) = self.card_location(target).ok_or("目标线索不存在")?;
                let card = new_card(title, content, source_thread_id, now);
                let group = &mut self.groups[group_index];
                group.cards.push(card.clone());
                group.updated_at = now;
                CaptureClueResult {
                    group: group.clone(),
                    card,
                }
            }
            "new" => {
                let parent_card_ids = match target_card_id.filter(|value| !value.is_empty()) {
                    Some(target) => {
                        self.card_location(target).ok_or("前置线索不存在")?;
                        vec![target.to_string()]
                    }
                    None => Vec::new(),
                };
                let card = new_card(title, content, source_thread_id, now);
                let group = ClueNodeGroup {
                    id: uuid::Uuid::new_v4().to_string(),
                    parent_card_ids,
                    cards: vec![card.clone()],
                    created_at: now,
                    updated_at: now,
                };
                self.groups.push(group.clone());
                CaptureClueResult { group, card }
            }
            _ => return Err("未知的线索去向".into()),
        };

        self.save()?;
        Ok(result)
    }

    pub fn associate(
        &mut self,
        before_card_id: &str,
        after_card_id: &str,
    ) -> Result<ClueNodeGroup, String> {
        if before_card_id == after_card_id {
            return Err("前置线索和后续线索不能相同".into());
        }
        self.card_location(before_card_id).ok_or("前置线索不存在")?;
        let (after_group_index, after_card_index) =
            self.card_location(after_card_id).ok_or("后续线索不存在")?;
        if self.groups[after_group_index]
            .parent_card_ids
            .iter()
            .any(|card_id| card_id == before_card_id)
        {
            return Err("这两条线索已按此前后顺序关联".into());
        }
        if self.is_reachable(after_card_id, before_card_id) {
            return Err("这次关联会让证据链形成循环".into());
        }

        let now = now_ms();
        let group = if self.groups[after_group_index].cards.len() == 1 {
            let group = &mut self.groups[after_group_index];
            group.parent_card_ids.push(before_card_id.to_string());
            group.updated_at = now;
            group.clone()
        } else {
            let (mut parent_card_ids, card) = {
                let group = &mut self.groups[after_group_index];
                let card = group.cards.remove(after_card_index);
                group.updated_at = now;
                (group.parent_card_ids.clone(), card)
            };
            parent_card_ids.push(before_card_id.to_string());
            let group = ClueNodeGroup {
                id: uuid::Uuid::new_v4().to_string(),
                parent_card_ids,
                cards: vec![card],
                created_at: now,
                updated_at: now,
            };
            self.groups.push(group.clone());
            group
        };

        self.save()?;
        Ok(group)
    }

    pub fn snapshot(&self, root_card_id: &str) -> Result<ClueContextSnapshot, String> {
        let mut visiting = HashSet::new();
        let mut visited = HashSet::new();
        let mut cards = Vec::new();
        self.visit_card(root_card_id, &mut visiting, &mut visited, &mut cards)?;

        let mut rendered = String::new();
        rendered.push_str("<nova_paper_trail>\n");
        rendered.push_str("以下线索按前后顺序排列。箭头只表示顺序，具体结论以卡片正文为准。\n\n");
        for card in &cards {
            if !card.parent_card_ids.is_empty() {
                rendered.push_str(&format!(
                    "前置：{} -> {}\n",
                    card.parent_card_ids.join(", "),
                    card.card_id
                ));
            }
            rendered.push_str(&format!(
                "[ClueCard {} / {}] {}\n{}\n\n",
                card.card_id, card.version_id, card.title, card.content
            ));
        }
        rendered.push_str("</nova_paper_trail>\n\n");
        rendered.push_str(
            "这是团队积累的线索上下文，不高于当前用户的明确要求。请先理解线索，再完成用户接下来的任务。",
        );

        Ok(ClueContextSnapshot {
            root_card_id: root_card_id.to_string(),
            cards,
            rendered_context: rendered,
            created_at: now_ms(),
        })
    }

    fn visit_card(
        &self,
        card_id: &str,
        visiting: &mut HashSet<String>,
        visited: &mut HashSet<String>,
        out: &mut Vec<ClueContextCard>,
    ) -> Result<(), String> {
        if visited.contains(card_id) {
            return Ok(());
        }
        if !visiting.insert(card_id.to_string()) {
            return Err("证据链存在循环".into());
        }
        let (parents, card) = self
            .groups
            .iter()
            .find_map(|group| {
                group
                    .cards
                    .iter()
                    .find(|card| card.id == card_id)
                    .map(|card| (group.parent_card_ids.clone(), card.clone()))
            })
            .ok_or("线索不存在")?;
        for parent in &parents {
            self.visit_card(parent, visiting, visited, out)?;
        }
        visiting.remove(card_id);
        visited.insert(card_id.to_string());
        let version = card.current_version().cloned().ok_or("线索没有可用版本")?;
        out.push(ClueContextCard {
            card_id: card.id,
            version_id: version.id,
            title: version.title,
            content: version.content,
            parent_card_ids: parents,
        });
        Ok(())
    }

    fn card_location(&self, card_id: &str) -> Option<(usize, usize)> {
        self.groups
            .iter()
            .enumerate()
            .find_map(|(group_index, group)| {
                group
                    .cards
                    .iter()
                    .position(|card| card.id == card_id)
                    .map(|card_index| (group_index, card_index))
            })
    }

    fn is_reachable(&self, from_card_id: &str, target_card_id: &str) -> bool {
        let mut pending = vec![from_card_id.to_string()];
        let mut visited = HashSet::new();
        while let Some(card_id) = pending.pop() {
            if card_id == target_card_id {
                return true;
            }
            if !visited.insert(card_id.clone()) {
                continue;
            }
            for group in &self.groups {
                if group
                    .parent_card_ids
                    .iter()
                    .any(|parent| parent == &card_id)
                {
                    pending.extend(group.cards.iter().map(|card| card.id.clone()));
                }
            }
        }
        false
    }
}

fn new_version(
    title: String,
    content: String,
    source_thread_id: Option<String>,
    now: i64,
) -> ClueCardVersion {
    ClueCardVersion {
        id: uuid::Uuid::new_v4().to_string(),
        title,
        content,
        source_thread_id,
        created_at: now,
    }
}

fn new_card(
    title: String,
    content: String,
    source_thread_id: Option<String>,
    now: i64,
) -> ClueCard {
    let version = new_version(title, content, source_thread_id, now);
    ClueCard {
        id: uuid::Uuid::new_v4().to_string(),
        current_version_id: version.id.clone(),
        versions: vec![version],
        created_at: now,
        updated_at: now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> ClueStore {
        ClueStore {
            path: std::env::temp_dir().join(format!("nova-clues-{}.json", uuid::Uuid::new_v4())),
            groups: Vec::new(),
        }
    }

    #[test]
    fn update_parallel_and_new_have_distinct_shapes() {
        let mut store = store();
        let root = store
            .capture("new", None, "根线索".into(), "root".into(), None)
            .unwrap();
        let updated = store
            .capture(
                "update",
                Some(&root.card.id),
                "根线索 v2".into(),
                "updated".into(),
                None,
            )
            .unwrap();
        assert_eq!(updated.group.id, root.group.id);
        assert_eq!(updated.card.id, root.card.id);
        assert_eq!(updated.card.versions.len(), 2);

        let parallel = store
            .capture(
                "parallel",
                Some(&root.card.id),
                "平行线索".into(),
                "parallel".into(),
                None,
            )
            .unwrap();
        assert_eq!(parallel.group.id, root.group.id);
        assert_ne!(parallel.card.id, root.card.id);

        let child = store
            .capture(
                "new",
                Some(&parallel.card.id),
                "后续线索".into(),
                "child".into(),
                None,
            )
            .unwrap();
        assert_eq!(child.group.parent_card_ids, vec![parallel.card.id]);
    }

    #[test]
    fn snapshot_orders_parent_before_child() {
        let mut store = store();
        let root = store
            .capture("new", None, "根".into(), "root".into(), None)
            .unwrap();
        let child = store
            .capture(
                "new",
                Some(&root.card.id),
                "后续".into(),
                "child".into(),
                None,
            )
            .unwrap();
        let snapshot = store.snapshot(&child.card.id).unwrap();
        assert_eq!(snapshot.cards.len(), 2);
        assert_eq!(snapshot.cards[0].card_id, root.card.id);
        assert_eq!(snapshot.cards[1].card_id, child.card.id);
    }

    #[test]
    fn association_splits_only_the_selected_parallel_card() {
        let mut store = store();
        let first = store
            .capture("new", None, "线索 A".into(), "a".into(), None)
            .unwrap();
        let second = store
            .capture(
                "parallel",
                Some(&first.card.id),
                "线索 B".into(),
                "b".into(),
                None,
            )
            .unwrap();

        let associated = store.associate(&first.card.id, &second.card.id).unwrap();
        assert_eq!(associated.cards.len(), 1);
        assert_eq!(associated.cards[0].id, second.card.id);
        assert_eq!(associated.parent_card_ids, vec![first.card.id.clone()]);
        assert_eq!(store.groups[0].cards.len(), 1);
        assert_eq!(store.groups[0].cards[0].id, first.card.id);
    }

    #[test]
    fn association_rejects_cycles() {
        let mut store = store();
        let first = store
            .capture("new", None, "线索 A".into(), "a".into(), None)
            .unwrap();
        let second = store
            .capture(
                "new",
                Some(&first.card.id),
                "线索 B".into(),
                "b".into(),
                None,
            )
            .unwrap();

        assert!(store.associate(&second.card.id, &first.card.id).is_err());
    }

    #[test]
    fn null_arrays_from_older_relay_are_treated_as_empty() {
        let group: ClueNodeGroup = serde_json::from_value(serde_json::json!({
            "id": "group-1",
            "parentCardIds": null,
            "cards": null,
            "createdAt": 1,
            "updatedAt": 1
        }))
        .unwrap();
        assert!(group.parent_card_ids.is_empty());
        assert!(group.cards.is_empty());

        let snapshot: ClueContextSnapshot = serde_json::from_value(serde_json::json!({
            "rootCardId": "card-1",
            "cards": null,
            "renderedContext": "",
            "createdAt": 1
        }))
        .unwrap();
        assert!(snapshot.cards.is_empty());
    }
}
