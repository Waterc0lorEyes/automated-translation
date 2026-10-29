use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub password_hash: String,
    pub api_key: Option<String>,
    #[allow(dead_code)]
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct Translation {
    pub id: i64,
    pub user_id: i64,
    pub filename: String,
    pub source_text: String,
    pub translated_text: String,
    pub source_lang: String,
    pub target_lang: String,
    /// queued / splitting / running / done / failed
    pub status: String,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Deserialize)]
pub struct RegisterForm {
    pub username: String,
    pub password: String,
    pub password_confirm: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginForm {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SubtitleEntry {
    pub index: usize,
    pub start: String,
    pub end: String,
    pub text: String,
    pub translation: Option<String>,
    pub note: Option<String>,
    /// VTT 时间轴行尾的定位信息，例如 "align:center line:81%"。
    /// SRT 没有这个字段；旧数据里也没有，故用 default 兼容。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AnnotateForm {
    pub topic: Option<String>,
    pub note_lang: Option<String>,
}

/// 文章按句拆分后的一个片段（没有时间轴，故不复用 SubtitleEntry）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArticleSegment {
    pub index: usize,
    pub text: String,
    pub translation: Option<String>,
    /// 注释（俚语、典故、文化背景等），旧数据没有该字段
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_rows_without_settings_still_parse() {
        // 修复前入库的数据没有 settings 字段，反序列化不能失败
        let old = r#"{"index":1,"start":"00:00:01.000","end":"00:00:02.000",
                      "text":"hi","translation":"你好","note":null}"#;
        let e: SubtitleEntry = serde_json::from_str(old).unwrap();
        assert!(e.settings.is_none());
        assert_eq!(e.translation.as_deref(), Some("你好"));
    }

    #[test]
    fn missing_settings_is_not_serialized() {
        let e = SubtitleEntry {
            index: 1, start: "a".into(), end: "b".into(), text: "t".into(),
            translation: None, note: None, settings: None,
        };
        assert!(!serde_json::to_string(&e).unwrap().contains("settings"));
    }
}
