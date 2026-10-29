//! 三语支持：简体中文 / 繁体中文 / English。
//!
//! 语言来源优先级：显式选择的 `lang` Cookie > 浏览器 `Accept-Language` > 默认简体。
//! 浏览器检测在 `routes::detect_language` 中间件里完成，检测结果以 lang Cookie 的形式
//! 注入请求，因此各 handler 仍然只从 CookieJar 读取即可。

pub const DEFAULT_LANG: &str = "zh";
/// 支持的语言代码
pub const SUPPORTED: [&str; 4] = ["zh", "zh-TW", "zh-HK", "en"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    ZhHans,
    /// 台湾繁体
    ZhHant,
    /// 香港繁体
    ZhHK,
    En,
}

impl Lang {
    pub fn from_code(code: &str) -> Self {
        match code.trim().to_ascii_lowercase().as_str() {
            "en" | "en-us" | "en-gb" => Lang::En,
            "zh-hk" | "zh-mo" => Lang::ZhHK,
            "zh-tw" | "zh-hant" => Lang::ZhHant,
            _ => Lang::ZhHans,
        }
    }

    fn idx(self) -> usize {
        match self {
            Lang::ZhHans => 0,
            Lang::ZhHant => 1,
            Lang::ZhHK => 2,
            Lang::En => 3,
        }
    }
}

pub struct I18n {
    lang: Lang,
}

impl I18n {
    pub fn new(code: &str) -> Self {
        I18n { lang: Lang::from_code(code) }
    }

    /// 未知 key 原样返回，返回值的生命周期跟随入参
    pub fn t<'a>(&self, key: &'a str) -> &'a str {
        get_translation(key, self.lang.idx())
    }
}

pub fn is_supported(code: &str) -> bool {
    let lower = code.trim().to_ascii_lowercase();
    SUPPORTED.iter().any(|s| s.to_ascii_lowercase() == lower)
}

fn get_translation(key: &str, idx: usize) -> &str {
    match key {
        "nav_subtitle" => ["字幕翻译", "字幕翻譯", "字幕翻譯", "Subtitle"][idx],
        "nav_article" => ["文章翻译", "文章翻譯", "文章翻譯", "Article"][idx],
        "nav_history" => ["历史", "歷史", "歷史", "History"][idx],
        "nav_settings" => ["设置", "設定", "設定", "Settings"][idx],
        "nav_logout" => ["退出", "登出", "登出", "Logout"][idx],

        "title_subtitle" => ["字幕翻译 - AI 翻译", "字幕翻譯 - AI 翻譯", "字幕翻譯 - AI 翻譯", "Subtitle - AI Translate"][idx],
        "title_article" => ["文章翻译 - AI 翻译", "文章翻譯 - AI 翻譯", "文章翻譯 - AI 翻譯", "Article - AI Translate"][idx],
        "title_history" => ["翻译历史 - AI 翻译", "翻譯歷史 - AI 翻譯", "翻譯歷史 - AI 翻譯", "History - AI Translate"][idx],
        "title_settings" => ["设置 - AI 翻译", "設定 - AI 翻譯", "設定 - AI 翻譯", "Settings - AI Translate"][idx],
        "title_result" => ["翻译结果 - AI 翻译", "翻譯結果 - AI 翻譯", "翻譯結果 - AI 翻譯", "Result - AI Translate"][idx],
        "title_index" => ["首页 - AI 翻译", "首頁 - AI 翻譯", "首頁 - AI 翻譯", "Home - AI Translate"][idx],
        "title_login" => ["登录 - AI 翻译", "登入 - AI 翻譯", "登入 - AI 翻譯", "Login - AI Translate"][idx],
        "title_register" => ["注册 - AI 翻译", "註冊 - AI 翻譯", "註冊 - AI 翻譯", "Register - AI Translate"][idx],

        "hero_subtitle" => [
            "上传 VTT / SRT 字幕文件，AI 自动翻译为中文",
            "上傳 VTT / SRT 字幕檔案，AI 自動翻譯為中文",
            "上傳 VTT / SRT 字幕檔案，AI 自動翻譯為中文",
            "Upload VTT/SRT subtitle files for AI translation"][idx],
        "hero_article" => [
            "上传文本文件或粘贴内容，AI 自动翻译",
            "上傳文字檔案或貼上內容，AI 自動翻譯",
            "上傳文字檔案或貼上內容，AI 自動翻譯",
            "Upload text files or paste content for AI translation"][idx],
        "hero_index" => ["AI 字幕 & 文章翻译", "AI 字幕 & 文章翻譯", "AI 字幕 & 文章翻譯", "AI Subtitle & Article Translation"][idx],
        "hero_index_sub" => ["选择翻译模式开始使用", "選擇翻譯模式開始使用", "選擇翻譯模式開始使用", "Choose a translation mode to begin"][idx],

        "h1_trans_result" => ["翻译结果", "翻譯結果", "翻譯結果", "Translation Result"][idx],
        "h2_annot" => ["AI 注释", "AI 註解", "AI 註釋", "AI Annotations"][idx],
        "h2_compare" => ["字幕对照", "字幕對照", "字幕對照", "Subtitle Comparison"][idx],
        "h2_seg" => ["逐句对照", "逐句對照", "逐句對照", "Sentence Comparison"][idx],
        "h2_upload_sub" => ["上传字幕文件", "上傳字幕檔案", "上傳字幕檔案", "Upload Subtitle"][idx],
        "h2_upload_art" => ["上传文章", "上傳文章", "上傳文章", "Upload Article"][idx],
        "h2_recent" => ["最近翻译", "最近翻譯", "最近翻譯", "Recent Translations"][idx],
        "h1_login" => ["登录", "登入", "登入", "Login"][idx],
        "h1_register" => ["注册", "註冊", "註冊", "Register"][idx],
        "h1_settings" => ["API 设置", "API 設定", "API 設定", "API Settings"][idx],
        "h1_history" => ["翻译历史", "翻譯歷史", "翻譯歷史", "Translation History"][idx],

        "label_source" => ["源语言", "來源語言", "來源語言", "Source"][idx],
        "label_target" => ["目标语言", "目標語言", "目標語言", "Target"][idx],
        "label_file" => [
            "上传字幕文件 (VTT/SRT)：",
            "上傳字幕檔案 (VTT/SRT)：",
            "上傳字幕檔案 (VTT/SRT)：",
            "Upload subtitle file (VTT/SRT):"][idx],
        "label_file_art" => ["上传文本文件 (TXT)：", "上傳文字檔案 (TXT)：", "上傳文字檔案 (TXT)：", "Upload text file (TXT):"][idx],
        "label_paste" => ["粘贴字幕内容：", "貼上字幕內容：", "貼上字幕內容：", "Paste subtitle content:"][idx],
        "label_paste_art" => ["粘贴文章内容：", "貼上文章內容：", "貼上文章內容：", "Paste article content:"][idx],
        "label_prompt" => ["自定义翻译提示词（可选）：", "自訂翻譯提示詞（選填）：", "自訂翻譯提示詞（選填）：", "Custom prompt (optional):"][idx],
        "label_username" => ["用户名", "使用者名稱", "用戶名稱", "Username"][idx],
        "label_password" => ["密码", "密碼", "密碼", "Password"][idx],
        "label_confirm" => ["确认密码", "確認密碼", "確認密碼", "Confirm Password"][idx],
        "drop_hint" => [
            "拖拽文件到此处，或点击选择",
            "拖曳檔案到此處，或點擊選擇",
            "拖曳檔案到此處，或點擊選取",
            "Drop a file here, or click to choose"][idx],
        "label_api_key" => ["DeepSeek API Key", "DeepSeek API Key", "DeepSeek API Key", "DeepSeek API Key"][idx],
        "label_annot_topic" => [
            "注释主题，如：俚语、文化背景...",
            "註解主題，如：俚語、文化背景...",
            "註釋主題，如：俚語、文化背景...",
            "Annotation topic, e.g. slang, culture..."][idx],
        "label_annot_lang" => ["中文注释", "中文註解", "中文註釋", "Chinese notes"][idx],
        "label_annot_lang_en" => ["English notes", "English notes", "English notes", "English notes"][idx],
        "label_annot_lang_ja" => ["日本語", "日本語", "日本語", "日本語"][idx],

        "btn_translate" => ["开始翻译", "開始翻譯", "開始翻譯", "Translate"][idx],
        "btn_login" => ["登录", "登入", "登入", "Login"][idx],
        "btn_register" => ["注册", "註冊", "註冊", "Register"][idx],
        "btn_save" => ["保存", "儲存", "儲存", "Save"][idx],
        "btn_save_edit" => ["保存修改", "儲存修改", "儲存修改", "Save Changes"][idx],
        "btn_cancel" => ["取消", "取消", "取消", "Cancel"][idx],
        "btn_annot_gen" => ["生成注释", "產生註解", "產生註釋", "Generate Notes"][idx],
        "btn_continue" => ["补译未匹配条目", "補譯未匹配條目", "補譯未匹配條目", "Fill missing"][idx],
        "btn_download_vtt" => ["下载 VTT", "下載 VTT", "下載 VTT", "Download VTT"][idx],
        "btn_download_srt" => ["下载 SRT", "下載 SRT", "下載 SRT", "Download SRT"][idx],
        "btn_download_txt" => ["下载 TXT", "下載 TXT", "下載 TXT", "Download TXT"][idx],
        "btn_dl_trans" => ["译文分句", "譯文分句", "譯文分句", "Translated sentences"][idx],
        "btn_dl_source" => ["原文分句", "原文分句", "原文分句", "Original sentences"][idx],
        "btn_dl_table" => ["对照表", "對照表", "對照表", "Comparison table"][idx],
        "btn_edit" => ["编辑", "編輯", "編輯", "Edit"][idx],
        "btn_start_sub" => ["开始", "開始", "開始", "Start"][idx],
        "btn_start_art" => ["开始", "開始", "開始", "Start"][idx],

        "th_index" => ["#", "#", "#", "#"][idx],
        "th_time" => ["时间", "時間", "時間", "Time"][idx],
        "th_original" => ["原文", "原文", "原文", "Original"][idx],
        "th_translation" => ["译文", "譯文", "譯文", "Translation"][idx],
        "th_note" => ["注释", "註解", "註釋", "Notes"][idx],
        "th_filename" => ["文件名", "檔名", "檔案名稱", "Filename"][idx],
        "th_lang" => ["语言", "語言", "語言", "Lang"][idx],
        "th_actions" => ["操作", "操作", "操作", "Actions"][idx],
        "th_view" => ["查看", "檢視", "檢視", "View"][idx],

        "divider_or" => ["或", "或", "或", "or"][idx],
        "file" => ["文件：", "檔案：", "檔案：", "File: "][idx],
        "time" => ["时间：", "時間：", "時間：", "Time: "][idx],

        "auth_no_account" => ["没有账号？", "沒有帳號？", "沒有帳戶？", "No account? "][idx],
        "auth_has_account" => ["已有账号？", "已有帳號？", "已有帳戶？", "Already have an account? "][idx],
        "auth_link" => ["注册", "註冊", "註冊", "Register"][idx],
        "auth_link_login" => ["登录", "登入", "登入", "Login"][idx],
        "auth_error_invalid" => ["用户名或密码错误", "使用者名稱或密碼錯誤", "用戶名稱或密碼錯誤", "Invalid username or password"][idx],
        "auth_error_empty" => ["请填写所有字段", "請填寫所有欄位", "請填寫所有欄位", "Please fill all fields"][idx],
        "auth_error_mismatch" => ["两次密码不一致", "兩次密碼不一致", "兩次密碼不一致", "Passwords do not match"][idx],
        "auth_error_short" => ["密码至少4个字符", "密碼至少 4 個字元", "密碼至少 4 個字元", "Password must be at least 4 characters"][idx],
        "auth_error_taken" => ["用户名已被注册", "使用者名稱已被註冊", "用戶名稱已被註冊", "Username already taken"][idx],
        "auth_error_db" => ["服务器错误，请重试", "伺服器錯誤，請重試", "伺服器錯誤，請重試", "Server error, please retry"][idx],
        "auth_register_success" => ["注册成功，请登录", "註冊成功，請登入", "註冊成功，請登入", "Registration successful, please login"][idx],

        "annot_failed" => ["注释生成失败，请重试", "註解產生失敗，請重試", "註釋產生失敗，請重試", "Annotation failed, please retry"][idx],
        "settings_saved" => ["设置已保存", "設定已儲存", "設定已儲存", "Settings saved"][idx],
        "settings_current" => ["当前: ", "目前：", "目前：", "Current: "][idx],
        "settings_get_key" => ["获取 DeepSeek API Key", "取得 DeepSeek API Key", "取得 DeepSeek API Key", "Get DeepSeek API Key"][idx],
        "no_translations" => ["暂无翻译记录", "尚無翻譯紀錄", "尚無翻譯紀錄", "No translations yet"][idx],
        "no_content" => ["请上传文件或粘贴内容", "請上傳檔案或貼上內容", "請上傳檔案或貼上內容", "Please upload a file or paste content"][idx],
        "no_api_key" => [
            "请先在设置页配置 API Key",
            "請先在設定頁設定 API Key",
            "請先在設定頁設定 API Key",
            "Configure API Key in settings first"][idx],
        "saving" => ["保存中...", "儲存中...", "儲存中...", "Saving..."][idx],
        "saved" => ["已保存", "已儲存", "已儲存", "Saved"][idx],
        "save_failed" => ["保存失败", "儲存失敗", "儲存失敗", "Save failed"][idx],

        "splitting_msg" => [
            "AI 正在拆分文章，完成后会自动刷新…",
            "AI 正在拆分文章，完成後會自動重新整理…",
            "AI 正在拆分文章，完成後會自動重新整理…",
            "AI is splitting the article, the page will refresh automatically…"][idx],
        "task_failed" => ["处理失败，请重新上传试试", "處理失敗，請重新上傳試試", "處理失敗，請重新上傳試試", "Processing failed, please upload again"][idx],
        "legacy_msg" => [
            "该记录是旧版本格式，已无法显示，请重新上传",
            "該紀錄是舊版本格式，已無法顯示，請重新上傳",
            "該紀錄是舊版本格式，已無法顯示，請重新上傳",
            "This record uses a legacy format and can no longer be displayed"][idx],
        "busy_msg" => [
            "有任务正在运行，请等它结束后再试",
            "有任務正在執行，請等它結束後再試",
            "有任務正在執行，請等它結束後再試",
            "A task is running, please wait until it finishes"][idx],

        _ => key,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_languages_resolve_distinctly() {
        let key = "nav_history";
        assert_eq!(I18n::new("zh").t(key), "历史");
        assert_eq!(I18n::new("zh-TW").t(key), "歷史");
        assert_eq!(I18n::new("zh-HK").t(key), "歷史");
        assert_eq!(I18n::new("en").t(key), "History");
    }

    #[test]
    fn hong_kong_uses_its_own_wording() {
        // 港式用词：帳戶 / 用戶 / 註釋
        assert_eq!(I18n::new("zh-TW").t("auth_no_account"), "沒有帳號？");
        assert_eq!(I18n::new("zh-HK").t("auth_no_account"), "沒有帳戶？");

        assert_eq!(I18n::new("zh-TW").t("label_username"), "使用者名稱");
        assert_eq!(I18n::new("zh-HK").t("label_username"), "用戶名稱");

        assert_eq!(I18n::new("zh-TW").t("h2_annot"), "AI 註解");
        assert_eq!(I18n::new("zh-HK").t("h2_annot"), "AI 註釋");
    }

    #[test]
    fn unknown_language_falls_back_to_simplified() {
        assert_eq!(I18n::new("fr").t("nav_history"), "历史");
        assert_eq!(I18n::new("").t("nav_history"), "历史");
    }

    #[test]
    fn unknown_key_returns_itself() {
        assert_eq!(I18n::new("zh").t("no_such_key"), "no_such_key");
    }







    #[test]
    fn supported_codes_are_recognised() {
        assert!(is_supported("zh"));
        assert!(is_supported("zh-TW"));
        assert!(is_supported("zh-HK"));
        assert!(is_supported("EN"));
        assert!(!is_supported("zh-TW-x"));
        assert!(!is_supported("fr"));
    }
}
