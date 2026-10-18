//! Message content: classification, formatted text <-> HTML, and turning
//! stored content back into sendable messages.

use tdrust::types::{
    FormattedText, InputAnimation, InputAudio, InputDocument, InputFile, InputFileRemote, InputMessageAnimation,
    InputMessageAudio, InputMessageContent, InputMessageDocument, InputMessagePhoto, InputMessageSticker, InputMessageVideo,
    InputMessageVideoNote, InputMessageVoiceNote, InputPhoto, InputSticker, InputVideo, InputVideoNote, InputVoiceNote,
    Message, MessageContent, TextEntityType,
};

use crate::model::{Content, MediaKind};
use crate::text::h;

/// The text of a message, or the caption of media.
pub fn text_of(content: &MessageContent) -> Option<&FormattedText> {
    Some(match content {
        MessageContent::Text(m) => &m.text,
        MessageContent::Photo(m) => &m.caption,
        MessageContent::Video(m) => &m.caption,
        MessageContent::Animation(m) => &m.caption,
        MessageContent::Audio(m) => &m.caption,
        MessageContent::Document(m) => &m.caption,
        MessageContent::VoiceNote(m) => &m.caption,
        _ => return None,
    })
}

/// Media kind and remote file id of a message.
pub fn media_of(content: &MessageContent) -> Option<(MediaKind, String)> {
    Some(match content {
        MessageContent::Photo(m) => (MediaKind::Photo, m.photo.sizes.last()?.photo.remote.id.clone()),
        MessageContent::Video(m) => (MediaKind::Video, m.video.video.remote.id.clone()),
        MessageContent::Animation(m) => (MediaKind::Animation, m.animation.animation.remote.id.clone()),
        MessageContent::Audio(m) => (MediaKind::Audio, m.audio.audio.remote.id.clone()),
        MessageContent::VoiceNote(m) => (MediaKind::Voice, m.voice_note.voice.remote.id.clone()),
        MessageContent::VideoNote(m) => (MediaKind::VideoNote, m.video_note.video.remote.id.clone()),
        MessageContent::Sticker(m) => (MediaKind::Sticker, m.sticker.sticker.remote.id.clone()),
        MessageContent::Document(m) => (MediaKind::Document, m.document.document.remote.id.clone()),
        _ => return None,
    })
}

pub fn is_media(content: &MessageContent) -> bool {
    media_of(content).is_some()
}

/// Service messages: joins, leaves, pins, title and photo changes, ...
pub fn is_service(content: &MessageContent) -> bool {
    matches!(
        content,
        MessageContent::ChatAddMembers(_)
            | MessageContent::ChatJoinByLink(_)
            | MessageContent::ChatJoinByRequest(_)
            | MessageContent::ChatDeleteMember(_)
            | MessageContent::ChatChangeTitle(_)
            | MessageContent::ChatChangePhoto(_)
            | MessageContent::ChatDeletePhoto(_)
            | MessageContent::PinMessage(_)
            | MessageContent::ChatSetTheme(_)
            | MessageContent::ChatSetBackground(_)
            | MessageContent::ChatSetMessageAutoDeleteTime(_)
            | MessageContent::ChatBoost(_)
            | MessageContent::ForumTopicCreated(_)
            | MessageContent::ForumTopicEdited(_)
            | MessageContent::ForumTopicIsClosedToggled(_)
            | MessageContent::ForumTopicIsHiddenToggled(_)
            | MessageContent::VideoChatScheduled(_)
            | MessageContent::VideoChatStarted(_)
            | MessageContent::VideoChatEnded(_)
            | MessageContent::InviteVideoChatParticipants(_)
            | MessageContent::SupergroupChatCreate(_)
            | MessageContent::BasicGroupChatCreate(_)
            | MessageContent::ChatUpgradeFrom(_)
            | MessageContent::ChatUpgradeTo(_)
            | MessageContent::ScreenshotTaken(_)
            | MessageContent::GameScore(_)
            | MessageContent::ProximityAlertTriggered(_)
    )
}

/// Converts formatted text to Telegram HTML, keeping bold, italic, links,
/// spoilers, code, quotes and custom emoji.
pub fn entities_to_html(text: &FormattedText) -> String {
    let mut spans: Vec<(usize, usize, String, &'static str)> = text
        .entities
        .iter()
        .filter_map(|entity| {
            let (open, close): (String, &str) = match &entity.r#type {
                TextEntityType::Bold(_) => ("<b>".into(), "</b>"),
                TextEntityType::Italic(_) => ("<i>".into(), "</i>"),
                TextEntityType::Underline(_) => ("<u>".into(), "</u>"),
                TextEntityType::Strikethrough(_) => ("<s>".into(), "</s>"),
                TextEntityType::Spoiler(_) => ("<tg-spoiler>".into(), "</tg-spoiler>"),
                TextEntityType::Code(_) => ("<code>".into(), "</code>"),
                TextEntityType::Pre(_) => ("<pre>".into(), "</pre>"),
                TextEntityType::PreCode(p) => (format!("<pre><code class=\"language-{}\">", h(&p.language)), "</code></pre>"),
                TextEntityType::TextUrl(u) => (format!("<a href=\"{}\">", h(&u.url)), "</a>"),
                TextEntityType::MentionName(m) => (format!("<a href=\"tg://user?id={}\">", m.user_id), "</a>"),
                TextEntityType::BlockQuote(_) => ("<blockquote>".into(), "</blockquote>"),
                TextEntityType::ExpandableBlockQuote(_) => ("<blockquote expandable>".into(), "</blockquote>"),
                TextEntityType::CustomEmoji(e) => (format!("<tg-emoji emoji-id=\"{}\">", e.custom_emoji_id), "</tg-emoji>"),
                _ => return None,
            };
            let start = usize::try_from(entity.offset).ok()?;
            let end = start + usize::try_from(entity.length).ok()?;
            Some((start, end, open, close))
        })
        .collect();
    // Outer spans first: earlier start, then longer.
    spans.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));

    let mut out = String::with_capacity(text.text.len() + spans.len() * 8);
    let mut stack: Vec<(usize, &str)> = Vec::new();
    let mut next = 0;
    let mut pos = 0;
    for c in text.text.chars() {
        close_spans(&mut out, &mut stack, pos);
        while next < spans.len() && spans[next].0 <= pos {
            let (_, end, open, close) = &spans[next];
            // Clamp to the enclosing span so tags always nest.
            let end = stack.last().map_or(*end, |(parent_end, _)| (*end).min(*parent_end));
            out.push_str(open);
            stack.push((end, close));
            next += 1;
        }
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
        pos += c.len_utf16();
    }
    close_spans(&mut out, &mut stack, usize::MAX);
    out
}

fn close_spans(out: &mut String, stack: &mut Vec<(usize, &str)>, pos: usize) {
    while stack.last().is_some_and(|(end, _)| *end <= pos) {
        let (_, close) = stack.pop().expect("checked above");
        out.push_str(close);
    }
}

/// Parses bot HTML into formatted text, falling back to the raw text if the
/// markup is invalid (so a broken custom text never silences the bot).
pub fn html(text: &str) -> FormattedText {
    FormattedText::html(text).unwrap_or_else(|err| {
        log::warn!("invalid HTML in a bot text ({err}): {text}");
        FormattedText::plain(text)
    })
}

/// The storable form of a message: its formatted text or its media.
pub fn to_content(message: &Message) -> Option<Content> {
    let caption = text_of(&message.content).map(entities_to_html).unwrap_or_default();
    if let Some((kind, file_id)) = media_of(&message.content) {
        return Some(Content::Media { kind, file_id, caption });
    }
    matches!(message.content, MessageContent::Text(_)).then_some(Content::Text(caption))
}

/// Builds a sendable message from stored content. `render` fills
/// placeholders in the text or caption.
pub fn input_content(content: &Content, render: impl Fn(&str) -> String) -> InputMessageContent {
    let (kind, file_id, caption) = match content {
        Content::Text(text) => return InputMessageContent::text(html(&render(text))),
        Content::Media { kind, file_id, caption } => (*kind, file_id, caption),
    };
    let file: InputFile = InputFileRemote { id: file_id.clone() }.into();
    let caption = (!caption.is_empty()).then(|| html(&render(caption)));
    input_media(kind, file, caption)
}

/// A media message of `kind` from any file: remote id, or a local path when
/// the file id belongs to another bot.
pub fn input_media(kind: MediaKind, file: InputFile, caption: Option<FormattedText>) -> InputMessageContent {
    match kind {
        MediaKind::Photo => InputMessagePhoto {
            photo: InputPhoto { photo: file, thumbnail: None, video: None, added_sticker_file_ids: vec![], width: 0, height: 0 },
            caption,
            show_caption_above_media: false,
            self_destruct_type: None,
            has_spoiler: false,
        }
        .into(),
        MediaKind::Video => InputMessageVideo {
            video: InputVideo {
                video: file,
                thumbnail: None,
                cover: None,
                start_timestamp: 0,
                added_sticker_file_ids: vec![],
                duration: 0,
                width: 0,
                height: 0,
                supports_streaming: true,
            },
            caption,
            show_caption_above_media: false,
            self_destruct_type: None,
            has_spoiler: false,
        }
        .into(),
        MediaKind::Animation => InputMessageAnimation {
            animation: InputAnimation {
                animation: file,
                thumbnail: None,
                added_sticker_file_ids: vec![],
                duration: 0,
                width: 0,
                height: 0,
            },
            caption,
            show_caption_above_media: false,
            has_spoiler: false,
        }
        .into(),
        MediaKind::Audio => InputMessageAudio {
            audio: InputAudio {
                audio: file,
                album_cover_thumbnail: None,
                duration: 0,
                title: String::new(),
                performer: String::new(),
            },
            caption,
        }
        .into(),
        MediaKind::Voice => InputMessageVoiceNote {
            voice_note: InputVoiceNote { voice_note: file, duration: 0, waveform: vec![] },
            caption,
            self_destruct_type: None,
        }
        .into(),
        MediaKind::VideoNote => InputMessageVideoNote {
            video_note: InputVideoNote { video_note: file, thumbnail: None, duration: 0, length: 0 },
            self_destruct_type: None,
        }
        .into(),
        MediaKind::Sticker => InputMessageSticker {
            sticker: InputSticker { sticker: file, thumbnail: None, width: 0, height: 0 },
            emoji: String::new(),
        }
        .into(),
        MediaKind::Document => InputMessageDocument {
            document: InputDocument { document: file, thumbnail: None, disable_content_type_detection: false },
            caption,
        }
        .into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tdrust::types::{TextEntity, TextEntityTypeBold, TextEntityTypeItalic, TextEntityTypeTextUrl};

    fn entity(offset: i32, length: i32, r#type: TextEntityType) -> TextEntity {
        TextEntity { offset, length, r#type }
    }

    #[test]
    fn converts_nested_entities() {
        let text = FormattedText {
            text: "Hello <world> link".into(),
            entities: vec![
                entity(0, 13, TextEntityTypeBold {}.into()),
                entity(6, 7, TextEntityTypeItalic {}.into()),
                entity(14, 4, TextEntityTypeTextUrl { url: "https://t.me".into() }.into()),
            ],
        };
        assert_eq!(entities_to_html(&text), "<b>Hello <i>&lt;world&gt;</i></b> <a href=\"https://t.me\">link</a>");
    }

    #[test]
    fn counts_offsets_in_utf16() {
        // The emoji takes two UTF-16 units.
        let text = FormattedText { text: "😀 سلام".into(), entities: vec![entity(3, 4, TextEntityTypeBold {}.into())] };
        assert_eq!(entities_to_html(&text), "😀 <b>سلام</b>");
    }
}
