use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use crate::threads::now_ms;

pub const EV_CLUES: &str = "clues:changed";
pub const EV_CLUE_MENTION_OPEN: &str = "clues:mention-open";

pub(crate) fn deserialize_vec_or_default<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<Vec<T>>::deserialize(deserializer).map(Option::unwrap_or_default)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ClueMention {
    pub token: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClueAttachment {
    pub name: String,
    pub mime_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClueCardVersion {
    pub id: String,
    pub title: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_thread_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_vec_or_default")]
    pub mentions: Vec<ClueMention>,
    #[serde(default, deserialize_with = "deserialize_vec_or_default")]
    pub attachments: Vec<ClueAttachment>,
    pub created_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClueComment {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_comment_id: Option<String>,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_token: Option<String>,
    pub author_name: String,
    #[serde(default, deserialize_with = "deserialize_vec_or_default")]
    pub mentions: Vec<ClueMention>,
    pub created_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClueCard {
    pub id: String,
    pub current_version_id: String,
    #[serde(default, deserialize_with = "deserialize_vec_or_default")]
    pub versions: Vec<ClueCardVersion>,
    #[serde(default, deserialize_with = "deserialize_vec_or_default")]
    pub comments: Vec<ClueComment>,
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
        author_name: String,
    ) -> Result<CaptureClueResult, String> {
        self.capture_with_mentions(
            placement,
            target_card_id,
            title,
            content,
            source_thread_id,
            author_name,
            Vec::new(),
        )
    }

    pub fn capture_with_mentions(
        &mut self,
        placement: &str,
        target_card_id: Option<&str>,
        title: String,
        content: String,
        source_thread_id: Option<String>,
        author_name: String,
        mentions: Vec<ClueMention>,
    ) -> Result<CaptureClueResult, String> {
        self.capture_with_attachments(
            placement,
            target_card_id,
            title,
            content,
            source_thread_id,
            author_name,
            mentions,
            Vec::new(),
        )
    }

    pub fn capture_with_attachments(
        &mut self,
        placement: &str,
        target_card_id: Option<&str>,
        title: String,
        content: String,
        source_thread_id: Option<String>,
        author_name: String,
        mentions: Vec<ClueMention>,
        attachments: Vec<ClueAttachment>,
    ) -> Result<CaptureClueResult, String> {
        let title = title.trim().to_string();
        let content = content.trim().to_string();
        if title.is_empty() {
            return Err("线索标题不能为空".into());
        }
        if content.is_empty() {
            return Err("线索内容不能为空".into());
        }
        let mentions = normalize_mentions(mentions, None);
        let now = now_ms();

        let result = match placement {
            "update" => {
                let target = target_card_id.ok_or("请选择要更新的线索")?;
                let (group_index, card_index) =
                    self.card_location(target).ok_or("目标线索不存在")?;
                let version = new_version(
                    title,
                    content,
                    source_thread_id,
                    author_name,
                    mentions,
                    attachments,
                    now,
                );
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
                let card = new_card(
                    title,
                    content,
                    source_thread_id,
                    author_name,
                    mentions,
                    attachments,
                    now,
                );
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
                let card = new_card(
                    title,
                    content,
                    source_thread_id,
                    author_name,
                    mentions,
                    attachments,
                    now,
                );
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

    pub fn add_comment(
        &mut self,
        card_id: &str,
        content: String,
        parent_comment_id: Option<String>,
        author_token: Option<String>,
        author_name: String,
        mut mentions: Vec<ClueMention>,
    ) -> Result<ClueComment, String> {
        let content = content.trim().to_string();
        if content.is_empty() {
            return Err("评论内容不能为空".into());
        }
        let (group_index, card_index) = self.card_location(card_id).ok_or("线索不存在")?;
        let parent = match parent_comment_id
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            Some(parent_id) => Some(
                self.groups[group_index].cards[card_index]
                    .comments
                    .iter()
                    .find(|comment| comment.id == parent_id)
                    .cloned()
                    .ok_or("回复的评论不存在")?,
            ),
            None => None,
        };
        if let Some(parent) = &parent {
            if let Some(token) = parent
                .author_token
                .as_deref()
                .filter(|token| !token.is_empty())
            {
                mentions.push(ClueMention {
                    token: token.to_string(),
                    name: parent.author_name.clone(),
                });
            }
        }
        let author_token = author_token
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty());
        let comment = ClueComment {
            id: uuid::Uuid::new_v4().to_string(),
            parent_comment_id: parent.map(|comment| comment.id),
            content,
            mentions: normalize_mentions(mentions, author_token.as_deref()),
            author_token,
            author_name: author_name.trim().to_string(),
            created_at: now_ms(),
        };
        self.groups[group_index].cards[card_index]
            .comments
            .push(comment.clone());
        self.save()?;
        Ok(comment)
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

    pub fn disassociate(
        &mut self,
        before_card_id: &str,
        after_card_id: &str,
    ) -> Result<ClueNodeGroup, String> {
        self.card_location(before_card_id).ok_or("前置线索不存在")?;
        let (after_group_index, _) = self.card_location(after_card_id).ok_or("后续线索不存在")?;
        let group = &mut self.groups[after_group_index];
        if !group
            .parent_card_ids
            .iter()
            .any(|card_id| card_id == before_card_id)
        {
            return Err("这两组线索没有该连接".into());
        }
        group
            .parent_card_ids
            .retain(|card_id| card_id != before_card_id);
        group.updated_at = now_ms();
        let result = group.clone();
        self.save()?;
        Ok(result)
    }

    pub fn split_card(&mut self, card_id: &str) -> Result<ClueNodeGroup, String> {
        let (group_index, card_index) = self.card_location(card_id).ok_or("线索不存在")?;
        if self.groups[group_index].cards.len() < 2 {
            return Err("这条线索没有与其它线索堆叠".into());
        }
        let now = now_ms();
        let (parent_card_ids, card) = {
            let group = &mut self.groups[group_index];
            let card = group.cards.remove(card_index);
            group.updated_at = now;
            (group.parent_card_ids.clone(), card)
        };
        let group = ClueNodeGroup {
            id: uuid::Uuid::new_v4().to_string(),
            parent_card_ids,
            cards: vec![card],
            created_at: now,
            updated_at: now,
        };
        self.groups.push(group.clone());
        self.save()?;
        Ok(group)
    }

    pub fn stack_cards(&mut self, card_ids: &[String]) -> Result<ClueNodeGroup, String> {
        let selected: HashSet<&str> = card_ids.iter().map(String::as_str).collect();
        if selected.len() < 2 {
            return Err("请至少选择两条线索进行堆叠".into());
        }

        let mut cards = Vec::with_capacity(selected.len());
        let mut parent_card_ids: Option<Vec<String>> = None;
        let mut expected_parents: Option<Vec<String>> = None;
        let mut source_group_ids = HashSet::new();
        for card_id in &selected {
            let (group_index, card_index) = self.card_location(card_id).ok_or("线索不存在")?;
            let group = &self.groups[group_index];
            let parents = group.parent_card_ids.clone();
            let mut canonical_parents = parents.clone();
            canonical_parents.sort();
            match &expected_parents {
                Some(expected) if expected != &canonical_parents => {
                    return Err("只有前置关系相同的线索才能堆叠".into());
                }
                None => {
                    expected_parents = Some(canonical_parents);
                    parent_card_ids = Some(parents);
                }
                _ => {}
            }
            source_group_ids.insert(group.id.clone());
            cards.push(group.cards[card_index].clone());
        }
        if source_group_ids.len() == 1 {
            let group = self
                .groups
                .iter()
                .find(|group| source_group_ids.contains(&group.id))
                .ok_or("线索组不存在")?;
            if group.cards.len() == selected.len() {
                return Err("所选线索已经堆叠在一起".into());
            }
        }

        cards.sort_by_key(|card| card.created_at);
        // 后续节点若同时挂着多张即将入堆的前置卡，折叠为堆内一张代表卡，避免同组多条重合连线
        let representative_id = cards
            .first()
            .map(|card| card.id.clone())
            .ok_or("线索不存在")?;
        let stacked_card_ids: HashSet<String> = cards.iter().map(|card| card.id.clone()).collect();
        let now = now_ms();
        for group in &mut self.groups {
            let original_len = group.cards.len();
            group
                .cards
                .retain(|card| !selected.contains(card.id.as_str()));
            if group.cards.len() != original_len {
                group.updated_at = now;
            }
        }
        self.groups.retain(|group| !group.cards.is_empty());
        for group in &mut self.groups {
            let mut saw_stacked_parent = false;
            let next_parents: Vec<String> = group
                .parent_card_ids
                .iter()
                .filter_map(|parent_id| {
                    if stacked_card_ids.contains(parent_id) {
                        saw_stacked_parent = true;
                        None
                    } else {
                        Some(parent_id.clone())
                    }
                })
                .collect();
            if !saw_stacked_parent {
                continue;
            }
            let mut merged = next_parents;
            if !merged
                .iter()
                .any(|parent_id| parent_id == &representative_id)
            {
                merged.push(representative_id.clone());
            }
            if merged != group.parent_card_ids {
                group.parent_card_ids = merged;
                group.updated_at = now;
            }
        }
        let group = ClueNodeGroup {
            id: uuid::Uuid::new_v4().to_string(),
            parent_card_ids: parent_card_ids.unwrap_or_default(),
            cards,
            created_at: now,
            updated_at: now,
        };
        self.groups.push(group.clone());
        self.save()?;
        Ok(group)
    }

    pub fn delete(&mut self, card_id: &str) -> Result<(), String> {
        let (group_index, card_index) = self.card_location(card_id).ok_or("线索不存在")?;
        let now = now_ms();
        if self.groups[group_index].cards.len() == 1 {
            self.groups.remove(group_index);
        } else {
            let group = &mut self.groups[group_index];
            group.cards.remove(card_index);
            group.updated_at = now;
        }
        for group in &mut self.groups {
            let parent_count = group.parent_card_ids.len();
            group.parent_card_ids.retain(|parent| parent != card_id);
            if group.parent_card_ids.len() != parent_count {
                group.updated_at = now;
            }
        }
        self.save()
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
    author_name: String,
    mentions: Vec<ClueMention>,
    attachments: Vec<ClueAttachment>,
    now: i64,
) -> ClueCardVersion {
    ClueCardVersion {
        id: uuid::Uuid::new_v4().to_string(),
        title,
        content,
        author_name: Some(author_name),
        source_thread_id,
        mentions,
        attachments,
        created_at: now,
    }
}

fn new_card(
    title: String,
    content: String,
    source_thread_id: Option<String>,
    author_name: String,
    mentions: Vec<ClueMention>,
    attachments: Vec<ClueAttachment>,
    now: i64,
) -> ClueCard {
    let version = new_version(
        title,
        content,
        source_thread_id,
        author_name,
        mentions,
        attachments,
        now,
    );
    ClueCard {
        id: uuid::Uuid::new_v4().to_string(),
        current_version_id: version.id.clone(),
        versions: vec![version],
        comments: Vec::new(),
        created_at: now,
        updated_at: now,
    }
}

fn normalize_mentions(
    mentions: Vec<ClueMention>,
    excluded_token: Option<&str>,
) -> Vec<ClueMention> {
    let mut seen = HashSet::new();
    mentions
        .into_iter()
        .filter_map(|mention| {
            let token = mention.token.trim().to_string();
            if token.is_empty()
                || excluded_token.is_some_and(|excluded| excluded == token)
                || !seen.insert(token.clone())
            {
                return None;
            }
            let name = mention.name.trim().to_string();
            Some(ClueMention {
                name: if name.is_empty() { token.clone() } else { name },
                token,
            })
        })
        .collect()
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
            .capture(
                "new",
                None,
                "根线索".into(),
                "root".into(),
                None,
                "甲".into(),
            )
            .unwrap();
        let updated = store
            .capture(
                "update",
                Some(&root.card.id),
                "根线索 v2".into(),
                "updated".into(),
                None,
                "乙".into(),
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
                "丙".into(),
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
                "丁".into(),
            )
            .unwrap();
        assert_eq!(child.group.parent_card_ids, vec![parallel.card.id]);
    }

    #[test]
    fn snapshot_orders_parent_before_child() {
        let mut store = store();
        let root = store
            .capture("new", None, "根".into(), "root".into(), None, "甲".into())
            .unwrap();
        let child = store
            .capture(
                "new",
                Some(&root.card.id),
                "后续".into(),
                "child".into(),
                None,
                "乙".into(),
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
            .capture("new", None, "线索 A".into(), "a".into(), None, "甲".into())
            .unwrap();
        let second = store
            .capture(
                "parallel",
                Some(&first.card.id),
                "线索 B".into(),
                "b".into(),
                None,
                "乙".into(),
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
            .capture("new", None, "线索 A".into(), "a".into(), None, "甲".into())
            .unwrap();
        let second = store
            .capture(
                "new",
                Some(&first.card.id),
                "线索 B".into(),
                "b".into(),
                None,
                "乙".into(),
            )
            .unwrap();

        assert!(store.associate(&second.card.id, &first.card.id).is_err());
    }

    #[test]
    fn disassociation_removes_shared_group_connection() {
        let mut store = store();
        let root = store
            .capture("new", None, "根".into(), "root".into(), None, "甲".into())
            .unwrap();
        let child = store
            .capture(
                "new",
                Some(&root.card.id),
                "后续".into(),
                "child".into(),
                None,
                "乙".into(),
            )
            .unwrap();
        let group = store.disassociate(&root.card.id, &child.card.id).unwrap();
        assert!(group.parent_card_ids.is_empty());
    }

    #[test]
    fn split_and_stack_only_move_selected_cards() {
        let mut store = store();
        let first = store
            .capture("new", None, "线索 A".into(), "a".into(), None, "甲".into())
            .unwrap();
        let second = store
            .capture(
                "parallel",
                Some(&first.card.id),
                "线索 B".into(),
                "b".into(),
                None,
                "乙".into(),
            )
            .unwrap();
        let third = store
            .capture(
                "parallel",
                Some(&first.card.id),
                "线索 C".into(),
                "c".into(),
                None,
                "丙".into(),
            )
            .unwrap();

        let split = store.split_card(&second.card.id).unwrap();
        assert_eq!(split.cards[0].id, second.card.id);
        let original = store
            .groups
            .iter()
            .find(|group| group.cards.iter().any(|card| card.id == first.card.id))
            .unwrap();
        assert_eq!(original.cards.len(), 2);

        let stacked = store
            .stack_cards(&[first.card.id.clone(), second.card.id.clone()])
            .unwrap();
        assert_eq!(stacked.cards.len(), 2);
        assert!(stacked.cards.iter().any(|card| card.id == first.card.id));
        assert!(stacked.cards.iter().any(|card| card.id == second.card.id));
        let remaining = store.card_location(&third.card.id).unwrap();
        assert_eq!(store.groups[remaining.0].cards.len(), 1);
    }

    #[test]
    fn stacking_rejects_different_parent_relationships() {
        let mut store = store();
        let root = store
            .capture("new", None, "根".into(), "root".into(), None, "甲".into())
            .unwrap();
        let child = store
            .capture(
                "new",
                Some(&root.card.id),
                "后续".into(),
                "child".into(),
                None,
                "乙".into(),
            )
            .unwrap();

        let result = store.stack_cards(&[root.card.id, child.card.id]);
        assert!(result.is_err());
        assert_eq!(store.groups.len(), 2);
    }

    #[test]
    fn stacking_parents_collapses_duplicate_child_edges() {
        let mut store = store();
        let left = store
            .capture("new", None, "左".into(), "left".into(), None, "甲".into())
            .unwrap();
        let right = store
            .capture("new", None, "右".into(), "right".into(), None, "乙".into())
            .unwrap();
        let child = store
            .capture(
                "new",
                Some(&left.card.id),
                "后续".into(),
                "child".into(),
                None,
                "丙".into(),
            )
            .unwrap();
        store.associate(&right.card.id, &child.card.id).unwrap();
        let before = store
            .groups
            .iter()
            .find(|group| group.cards.iter().any(|card| card.id == child.card.id))
            .unwrap();
        assert_eq!(before.parent_card_ids.len(), 2);

        let stacked = store
            .stack_cards(&[left.card.id.clone(), right.card.id.clone()])
            .unwrap();
        assert_eq!(stacked.cards.len(), 2);
        let after = store
            .groups
            .iter()
            .find(|group| group.cards.iter().any(|card| card.id == child.card.id))
            .unwrap();
        assert_eq!(after.parent_card_ids.len(), 1);
        assert!(stacked
            .cards
            .iter()
            .any(|card| after.parent_card_ids.contains(&card.id)));
    }

    #[test]
    fn stacking_siblings_keeps_single_parent() {
        let mut store = store();
        let root = store
            .capture("new", None, "根".into(), "root".into(), None, "甲".into())
            .unwrap();
        let left = store
            .capture(
                "new",
                Some(&root.card.id),
                "左".into(),
                "left".into(),
                None,
                "乙".into(),
            )
            .unwrap();
        let right = store
            .capture(
                "new",
                Some(&root.card.id),
                "右".into(),
                "right".into(),
                None,
                "丙".into(),
            )
            .unwrap();
        assert_eq!(store.groups.len(), 3);

        let stacked = store
            .stack_cards(&[left.card.id.clone(), right.card.id.clone()])
            .unwrap();
        assert_eq!(stacked.parent_card_ids, vec![root.card.id]);
        assert_eq!(store.groups.len(), 2);
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

        let card: ClueCard = serde_json::from_value(serde_json::json!({
            "id": "card-1",
            "currentVersionId": "version-1",
            "versions": [{
                "id": "version-1",
                "title": "旧线索",
                "content": "正文",
                "mentions": null,
                "createdAt": 1
            }],
            "comments": null,
            "createdAt": 1,
            "updatedAt": 1
        }))
        .unwrap();
        assert!(card.versions[0].mentions.is_empty());
        assert!(card.comments.is_empty());
    }

    #[test]
    fn mentions_belong_to_the_published_version_and_are_deduplicated() {
        let mut store = store();
        let root = store
            .capture("new", None, "线索".into(), "v1".into(), None, "甲".into())
            .unwrap();
        let updated = store
            .capture_with_mentions(
                "update",
                Some(&root.card.id),
                "线索".into(),
                "v2".into(),
                None,
                "乙".into(),
                vec![
                    ClueMention {
                        token: "peer-b".into(),
                        name: "成员乙".into(),
                    },
                    ClueMention {
                        token: "peer-b".into(),
                        name: "重复名称".into(),
                    },
                ],
            )
            .unwrap();

        assert!(updated.card.versions[0].mentions.is_empty());
        assert_eq!(updated.card.versions[1].mentions.len(), 1);
        assert_eq!(updated.card.versions[1].mentions[0].token, "peer-b");
        assert_eq!(updated.card.versions[1].mentions[0].name, "成员乙");
    }

    #[test]
    fn comments_and_replies_persist_and_reply_mentions_the_parent_author() {
        let mut store = store();
        let root = store
            .capture("new", None, "线索".into(), "正文".into(), None, "甲".into())
            .unwrap();
        let comment = store
            .add_comment(
                &root.card.id,
                "第一条评论".into(),
                None,
                Some("peer-a".into()),
                "甲".into(),
                vec![
                    ClueMention {
                        token: "peer-b".into(),
                        name: "乙".into(),
                    },
                    ClueMention {
                        token: "peer-b".into(),
                        name: "重复".into(),
                    },
                    ClueMention {
                        token: "peer-a".into(),
                        name: "甲".into(),
                    },
                ],
            )
            .unwrap();
        assert_eq!(comment.mentions.len(), 1);
        assert_eq!(comment.mentions[0].token, "peer-b");

        let reply = store
            .add_comment(
                &root.card.id,
                "回复".into(),
                Some(comment.id.clone()),
                Some("peer-b".into()),
                "乙".into(),
                Vec::new(),
            )
            .unwrap();
        assert_eq!(
            reply.parent_comment_id.as_deref(),
            Some(comment.id.as_str())
        );
        assert_eq!(reply.mentions.len(), 1);
        assert_eq!(reply.mentions[0].token, "peer-a");

        let self_reply = store
            .add_comment(
                &root.card.id,
                "自回复".into(),
                Some(comment.id.clone()),
                Some("peer-a".into()),
                "甲".into(),
                Vec::new(),
            )
            .unwrap();
        assert!(self_reply.mentions.is_empty());

        let (group_index, card_index) = store.card_location(&root.card.id).unwrap();
        assert_eq!(store.groups[group_index].updated_at, root.group.updated_at);
        assert_eq!(
            store.groups[group_index].cards[card_index].updated_at,
            root.card.updated_at
        );

        let persisted: ClueFile =
            serde_json::from_str(&fs::read_to_string(&store.path).unwrap()).unwrap();
        let card = persisted
            .groups
            .iter()
            .flat_map(|group| &group.cards)
            .find(|card| card.id == root.card.id)
            .unwrap();
        assert_eq!(card.comments.len(), 3);
        assert!(store
            .add_comment(
                &root.card.id,
                " ".into(),
                None,
                None,
                "甲".into(),
                Vec::new(),
            )
            .is_err());
        assert!(store
            .add_comment(
                &root.card.id,
                "回复不存在的评论".into(),
                Some("missing".into()),
                None,
                "甲".into(),
                Vec::new(),
            )
            .is_err());
    }

    #[test]
    fn delete_keeps_downstream_and_removes_parent_reference() {
        let mut store = store();
        let root = store
            .capture("new", None, "根".into(), "root".into(), None, "甲".into())
            .unwrap();
        let parallel = store
            .capture(
                "parallel",
                Some(&root.card.id),
                "平行".into(),
                "parallel".into(),
                None,
                "乙".into(),
            )
            .unwrap();
        let child = store
            .capture(
                "new",
                Some(&root.card.id),
                "后续".into(),
                "child".into(),
                None,
                "丙".into(),
            )
            .unwrap();

        store.delete(&root.card.id).unwrap();
        assert!(store.card_location(&root.card.id).is_none());
        assert!(store.card_location(&parallel.card.id).is_some());
        let (group_index, _) = store.card_location(&child.card.id).unwrap();
        assert!(store.groups[group_index].parent_card_ids.is_empty());
    }
}
