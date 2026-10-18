//! Conveniences on top of the generated API for the most common tasks.

use crate::functions::ParseTextEntities;
use crate::types::{
    FormattedText, InputMessageContent, InputMessageReplyToMessage, InputMessageText, Message, MessageContent,
    MessageSender, TextParseModeHTML, TextParseModeMarkdown,
};
use crate::{Client, Result};

impl FormattedText {
    /// Text without formatting.
    pub fn plain(text: impl Into<String>) -> Self {
        FormattedText { text: text.into(), entities: Vec::new() }
    }

    /// Parses [MarkdownV2](https://core.telegram.org/bots/api#markdownv2-style)
    /// (`*bold*`, `_italic_`, `` `code` ``, `[link](https://...)`, ...).
    ///
    /// Parsing runs locally in TDLib and does not need a client.
    pub fn markdown(text: impl Into<String>) -> Result<Self> {
        crate::execute(ParseTextEntities { text: text.into(), parse_mode: TextParseModeMarkdown { version: 2 }.into() })
    }

    /// Parses Telegram's [HTML subset](https://core.telegram.org/bots/api#html-style)
    /// (`<b>`, `<i>`, `<code>`, `<a href="...">`, ...).
    pub fn html(text: impl Into<String>) -> Result<Self> {
        crate::execute(ParseTextEntities { text: text.into(), parse_mode: TextParseModeHTML {}.into() })
    }
}

impl From<&str> for FormattedText {
    fn from(text: &str) -> Self {
        FormattedText::plain(text)
    }
}

impl From<String> for FormattedText {
    fn from(text: String) -> Self {
        FormattedText::plain(text)
    }
}

impl InputMessageContent {
    /// A text message.
    pub fn text(text: impl Into<FormattedText>) -> Self {
        InputMessageText { text: text.into(), ..Default::default() }.into()
    }
}

impl Message {
    /// The text of a text message; `None` for other kinds of content.
    pub fn text(&self) -> Option<&str> {
        match &self.content {
            MessageContent::Text(content) => Some(&content.text.text),
            _ => None,
        }
    }

    /// The user who sent the message, if it was sent by a user rather than
    /// on behalf of a chat.
    pub fn sender_user_id(&self) -> Option<i64> {
        self.sender_id.user_id()
    }
}

impl MessageSender {
    /// The user identifier, for messages sent by a user.
    pub fn user_id(&self) -> Option<i64> {
        match self {
            MessageSender::User(sender) => Some(sender.user_id),
            _ => None,
        }
    }

    /// The chat identifier, for messages sent on behalf of a chat.
    pub fn chat_id(&self) -> Option<i64> {
        match self {
            MessageSender::Chat(sender) => Some(sender.chat_id),
            _ => None,
        }
    }
}

impl Client {
    /// Sends a text message to a chat.
    ///
    /// Returns the message as TDLib first reports it, with a temporary
    /// identifier; the final message arrives in `Update::MessageSendSucceeded`.
    ///
    /// ```no_run
    /// # async fn run(client: tdrust::Client, chat_id: i64) -> tdrust::Result<()> {
    /// use tdrust::types::FormattedText;
    ///
    /// client.send_text(chat_id, "Hello!").await?;
    /// client.send_text(chat_id, FormattedText::markdown("*Bold* and _italic_")?).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send_text(&self, chat_id: i64, text: impl Into<FormattedText>) -> Result<Message> {
        self.send_message(chat_id, None, None, None, None, InputMessageContent::text(text)).await
    }

    /// Replies to a message with text, in the same chat and topic.
    pub async fn reply_text(&self, to: &Message, text: impl Into<FormattedText>) -> Result<Message> {
        let reply_to = InputMessageReplyToMessage { message_id: to.id, ..Default::default() };
        self.send_message(
            to.chat_id,
            to.topic_id.clone(),
            Some(reply_to.into()),
            None,
            None,
            InputMessageContent::text(text),
        )
        .await
    }
}
