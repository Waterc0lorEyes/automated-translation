use crate::models::SubtitleEntry;

pub fn parse_subtitles(content: &str) -> Vec<SubtitleEntry> {
    let content = content.trim();

    if content.starts_with("WEBVTT") {
        parse_vtt(content)
    } else if content.trim_start().starts_with("1\n") || content.contains("\n1\n") {
        parse_srt(content)
    } else {
        parse_vtt(content)
    }
}

fn parse_vtt(content: &str) -> Vec<SubtitleEntry> {
    let mut entries = Vec::new();
    let mut lines = content.lines().peekable();

    while let Some(line) = lines.next() {
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with("WEBVTT")
            || line.starts_with("Kind:")
            || line.starts_with("Language:")
            || line.starts_with("NOTE")
        {
            continue;
        }

        // 时间轴行形如：00:00:14.844 --> 00:00:16.804 align:center line:81%
        // "-->" 之后第一个 token 是结束时间，其余是定位信息，必须原样保留，
        // 否则导出的字幕在播放器里位置会变。
        let Some((start, rest)) = line.split_once("-->") else {
            continue;
        };
        let start = start.trim().to_string();
        let mut tokens = rest.split_whitespace();
        let end = tokens.next().unwrap_or("00:00:00.000").to_string();
        let settings = tokens.collect::<Vec<_>>().join(" ");
        let settings = if settings.is_empty() { None } else { Some(settings) };

        let mut text_lines = Vec::new();
        while let Some(&next_line) = lines.peek() {
            if next_line.trim().is_empty() || next_line.contains("-->") {
                break;
            }
            text_lines.push(next_line.to_string());
            lines.next();
        }

        // 用换行拼接以保留原始断行；匹配模型返回时再统一压成单行比较
        let text = text_lines.join("\n");
        if !text.trim().is_empty() {
            entries.push(SubtitleEntry {
                index: entries.len() + 1,
                start,
                end,
                text,
                translation: None,
                note: None,
                settings,
            });
        }
    }

    entries
}

fn parse_srt(content: &str) -> Vec<SubtitleEntry> {
    let mut entries = Vec::new();
    let mut lines = content.lines().peekable();

    while let Some(line) = lines.next() {
        if line.trim().is_empty() {
            continue;
        }

        let index: usize = line.trim().parse().unwrap_or(0);
        if index == 0 {
            continue;
        }

        let time_line = match lines.next() {
            Some(l) => l,
            None => break,
        };

        let parts: Vec<&str> = time_line.splitn(2, "-->").collect();
        let start = parts[0].trim().replace(',', ".").to_string();
        let end = parts.get(1)
            .map(|s| s.trim().replace(',', "."))
            .unwrap_or_default();

        let mut text_lines = Vec::new();
        while let Some(&next_line) = lines.peek() {
            if next_line.trim().is_empty() {
                lines.next();
                break;
            }
            text_lines.push((*next_line).to_string());
            lines.next();
        }

        let text = text_lines.join("\n");
        if !text.trim().is_empty() {
            entries.push(SubtitleEntry {
                index,
                start,
                end,
                text,
                translation: None,
                note: None,
                settings: None,
            });
        }
    }

    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    const VTT: &str = "WEBVTT\n\n\
        00:00:14.844 --> 00:00:16.804 align:center line:81%\n\
        How often the average day.\n\n\
        00:00:16.846 --> 00:00:21.100 align:center line:79%\n\
        The average week. Do you\n\
        think about the rule of law?\n";

    const SRT: &str = "1\n00:00:01,000 --> 00:00:02,500\nHello there\n\n\
        2\n00:00:03,000 --> 00:00:04,000\nSecond line\nwrapped\n";

    #[test]
    fn vtt_keeps_positioning_settings() {
        let entries = parse_vtt(VTT);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].settings.as_deref(), Some("align:center line:81%"));
        assert_eq!(entries[1].settings.as_deref(), Some("align:center line:79%"));
        assert_eq!(entries[0].start, "00:00:14.844");
        assert_eq!(entries[0].end, "00:00:16.804");
    }

    #[test]
    fn vtt_keeps_multiline_text() {
        let entries = parse_vtt(VTT);
        assert_eq!(entries[1].text, "The average week. Do you\nthink about the rule of law?");
    }

    #[test]
    fn srt_parses_and_normalizes_commas() {
        let entries = parse_srt(SRT);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].index, 1);
        assert_eq!(entries[0].start, "00:00:01.000");
        assert_eq!(entries[0].end, "00:00:02.500");
        assert_eq!(entries[1].text, "Second line\nwrapped");
        assert!(entries[0].settings.is_none());
    }

    #[test]
    fn dispatch_detects_format() {
        assert_eq!(parse_subtitles(VTT).len(), 2);
        assert_eq!(parse_subtitles(SRT).len(), 2);
    }

    #[test]
    fn vtt_without_settings_has_none() {
        let vtt = "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nPlain\n";
        let entries = parse_subtitles(vtt);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].settings.is_none());
    }
}
