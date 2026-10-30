use crate::error::Result;
use crate::store::{PutItem, Store, get_records, put_records};
use serde::{Deserialize, Serialize};

pub const BASE_SYSTEM_PROMPT: &str = "你是一个有用的助手。";
pub const SYSTEM_PROMPT_TEMPLATE: &str = "{{base_system_prompt}}\n\n当前可用的本地能力分类：\n{{categories}}\n\n可使用的技能：\n{{skills}}\n\n使用规则：\n{{rules}}";
pub const RULES: [&str; 4] = [
    "只使用与当前任务相关的技能；不确定时先追问。",
    "调用本地插件或修改文件前，先确认参数与路径。",
    "不编造执行结果；失败时返回可读错误。",
    "不输出 API Key、密码等敏感信息。",
];
pub const MAX_SYSTEM_CHARS: usize = 6000;
pub const MAX_SKILLS: usize = 20;

const PROMPT_COLLECTION: &str = "prompt";
const PROMPT_KEYS: [&str; 4] = [
    "base_system_prompt",
    "system_prompt_template",
    "rules",
    "limits",
];

#[derive(Debug, Clone, Deserialize)]
pub struct PromptSkill {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub sort: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PromptLimits {
    pub max_system_chars: usize,
    pub max_skills: usize,
}

impl Default for PromptLimits {
    fn default() -> Self {
        Self {
            max_system_chars: MAX_SYSTEM_CHARS,
            max_skills: MAX_SKILLS,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PromptBuild {
    pub system_prompt: String,
    pub skill_ids: Vec<String>,
    pub categories: Vec<String>,
    pub truncated: bool,
}

/// Insert the default prompt records if they do not exist yet. Existing values
/// are never overwritten, so prompt edits survive restarts.
pub fn seed(store: &Store) -> Result<()> {
    let defaults = [
        ("base_system_prompt", BASE_SYSTEM_PROMPT, "text"),
        ("system_prompt_template", SYSTEM_PROMPT_TEMPLATE, "text"),
        (
            "rules",
            "[\"只使用与当前任务相关的技能；不确定时先追问。\",\"调用本地插件或修改文件前，先确认参数与路径。\",\"不编造执行结果；失败时返回可读错误。\",\"不输出 API Key、密码等敏感信息。\"]",
            "json",
        ),
        (
            "limits",
            "{\"max_system_chars\":6000,\"max_skills\":20}",
            "json",
        ),
    ];

    for (key, value, value_type) in defaults {
        let existing = get_records(store, PROMPT_COLLECTION, &[key.to_string()], false)?;
        if existing.missing.is_empty() {
            continue;
        }
        let _ = put_records(
            store,
            PROMPT_COLLECTION,
            &[PutItem {
                key: key.to_string(),
                value: Some(value.to_string()),
                value_type: Some(value_type.to_string()),
                ..Default::default()
            }],
        )?;
    }
    Ok(())
}

pub fn load_defaults(store: &Store) -> Result<(String, String, Vec<String>, PromptLimits)> {
    seed(store)?;
    let keys = PROMPT_KEYS.map(ToString::to_string);
    let mut values = get_records(store, PROMPT_COLLECTION, &keys, false)?;
    // get_records preserves the request order only for found records; map by key.
    let mut map = std::collections::HashMap::new();
    for item in values.list.drain(..) {
        if let (Some(key), Some(value)) = (
            item.get("key").and_then(|value| value.as_str()),
            item.get("value").and_then(|value| value.as_str()),
        ) {
            map.insert(key.to_string(), value.to_string());
        }
    }
    let base = map
        .remove("base_system_prompt")
        .unwrap_or_else(|| BASE_SYSTEM_PROMPT.to_string());
    let template = map
        .remove("system_prompt_template")
        .unwrap_or_else(|| SYSTEM_PROMPT_TEMPLATE.to_string());
    let rules: Vec<String> = map
        .remove("rules")
        .and_then(|text| serde_json::from_str::<Vec<String>>(&text).ok())
        .unwrap_or_else(|| RULES.iter().map(ToString::to_string).collect());
    let limits: PromptLimits = map
        .remove("limits")
        .and_then(|text| serde_json::from_str::<PromptLimits>(&text).ok())
        .unwrap_or_default();
    Ok((base, template, rules, limits))
}

pub fn assemble(
    base: Option<&str>,
    template: &str,
    rules: &[String],
    limits: &PromptLimits,
    skills: &[PromptSkill],
) -> PromptBuild {
    let base = base
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or(BASE_SYSTEM_PROMPT);

    let mut selected: Vec<PromptSkill> = skills
        .iter()
        .filter(|skill| !skill.id.trim().is_empty() && !skill.content.trim().is_empty())
        .cloned()
        .collect();
    selected.sort_by(|a, b| {
        category_rank(&a.category)
            .cmp(&category_rank(&b.category))
            .then_with(|| a.sort.cmp(&b.sort))
            .then_with(|| a.name.cmp(&b.name))
    });

    let mut truncated = false;
    if selected.len() > limits.max_skills {
        selected.truncate(limits.max_skills);
        truncated = true;
    }

    let mut system_prompt = if selected.is_empty() {
        base.to_string()
    } else {
        compose(base, template, rules, &selected)
    };

    while system_prompt.chars().count() > limits.max_system_chars && !selected.is_empty() {
        selected.pop();
        truncated = true;
        system_prompt = if selected.is_empty() {
            base.to_string()
        } else {
            compose(base, template, rules, &selected)
        };
    }

    let mut categories: Vec<String> = selected
        .iter()
        .map(|skill| skill.category.clone())
        .collect();
    categories.sort_by_key(|category| category_rank(category));
    categories.dedup();

    PromptBuild {
        system_prompt,
        skill_ids: selected.iter().map(|skill| skill.id.clone()).collect(),
        categories,
        truncated,
    }
}

fn compose(base: &str, template: &str, rules: &[String], skills: &[PromptSkill]) -> String {
    let mut categories = Vec::new();
    for skill in skills {
        if !categories.contains(&skill.category) {
            categories.push(skill.category.clone());
        }
    }
    categories.sort_by_key(|category| category_rank(category));

    let category_lines = categories
        .iter()
        .map(|category| format!("- {}（{}）", category_label(category), category))
        .collect::<Vec<_>>()
        .join("\n");
    let skill_lines = skills
        .iter()
        .map(|skill| {
            format!(
                "- [{}] {}：{}",
                category_label(&skill.category),
                if skill.name.is_empty() {
                    &skill.id
                } else {
                    &skill.name
                },
                skill.content
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let rule_lines = rules
        .iter()
        .map(|rule| format!("- {rule}"))
        .collect::<Vec<_>>()
        .join("\n");

    template
        .replace("{{base_system_prompt}}", base)
        .replace("{{categories}}", &category_lines)
        .replace("{{skills}}", &skill_lines)
        .replace("{{rules}}", &rule_lines)
}

pub fn category_rank(category: &str) -> i64 {
    match category {
        "general" => 10,
        "design" => 20,
        "material" => 30,
        "plugin" => 40,
        "safety" => 90,
        _ => 100,
    }
}

pub fn category_label(category: &str) -> &str {
    match category {
        "general" => "通用",
        "design" => "设计软件",
        "material" => "素材库",
        "plugin" => "本地插件",
        "safety" => "安全与权限",
        _ => "其他",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(id: &str, category: &str, content: &str, sort: i64) -> PromptSkill {
        PromptSkill {
            id: id.into(),
            name: id.into(),
            category: category.into(),
            content: content.into(),
            sort,
        }
    }

    #[test]
    fn assemble_orders_and_truncates() {
        let limits = PromptLimits {
            max_system_chars: 400,
            max_skills: 2,
        };
        let skills = vec![
            skill("plugin-1", "plugin", "插件能力", 1),
            skill("design-1", "design", "设计规范", 2),
            skill("general-1", "general", "通用规范", 1),
        ];
        let build = assemble(
            Some("基础"),
            SYSTEM_PROMPT_TEMPLATE,
            &RULES.map(ToString::to_string),
            &limits,
            &skills,
        );
        assert!(build.truncated);
        assert_eq!(build.skill_ids, vec!["general-1", "design-1"]);
        assert!(build.system_prompt.contains("基础"));
        assert!(build.system_prompt.contains("[通用] general-1：通用规范"));
    }

    #[test]
    fn assemble_without_skills_returns_base() {
        let build = assemble(
            None,
            SYSTEM_PROMPT_TEMPLATE,
            &[],
            &PromptLimits::default(),
            &[],
        );
        assert_eq!(build.system_prompt, BASE_SYSTEM_PROMPT);
        assert!(build.skill_ids.is_empty());
        assert!(!build.truncated);
    }

    #[test]
    fn empty_content_is_dropped() {
        let build = assemble(
            None,
            SYSTEM_PROMPT_TEMPLATE,
            &[],
            &PromptLimits::default(),
            &[skill("empty", "general", "   ", 0)],
        );
        assert_eq!(build.system_prompt, BASE_SYSTEM_PROMPT);
    }
}
