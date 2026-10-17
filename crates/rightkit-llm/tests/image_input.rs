//! Image and frame input on the model client (Orthic-Labs/rightkit#298).

use std::sync::Arc;
use std::time::Duration;

use rightkit_http::testing::{MockResponse, MockServer};
use rightkit_http::{Client, ClientConfig, RetryPolicy};
use rightkit_llm::anthropic::{self, AnthropicProvider};
use rightkit_llm::credentials::{CredentialSource, StaticCredentials};
use rightkit_llm::openai::{self, MaxTokensField, OpenAiCompatProvider};
use rightkit_llm::{ChatRequest, ContentBlock, Frame, ImageSource, LlmError, Message, Provider};
use serde_json::json;

fn client() -> Client {
    Client::new(ClientConfig {
        retry: RetryPolicy::none(),
        ..rightkit_http::testing::loopback_config()
    })
}

fn creds(id: &str) -> Arc<dyn CredentialSource> {
    Arc::new(StaticCredentials::default().with(id, "k"))
}

fn request(message: Message) -> ChatRequest {
    ChatRequest::new("m", vec![message])
}

#[test]
fn anthropic_sends_base64_and_url_images_in_order() {
    let message = Message::user_blocks(vec![
        ContentBlock::text("what is this?"),
        ContentBlock::image_bytes("image/png", b"foo"),
        ContentBlock::image_url("https://example.com/a.jpg"),
    ]);
    let body = anthropic::build_body(&request(message), false);
    let content = &body["messages"][0]["content"];
    assert_eq!(content[0], json!({"type": "text", "text": "what is this?"}));
    assert_eq!(
        content[1],
        json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "Zm9v"}})
    );
    assert_eq!(
        content[2],
        json!({"type": "image", "source": {"type": "url", "url": "https://example.com/a.jpg"}})
    );
}

#[test]
fn openai_sends_an_image_message_as_content_parts() {
    let message = Message::user_with_images(
        "describe",
        vec![
            ImageSource::from_bytes("image/jpeg", b"foo"),
            ImageSource::url("https://example.com/a.png"),
        ],
    );
    let body = openai::build_body(&request(message), false, MaxTokensField::MaxTokens);
    let content = &body["messages"][0]["content"];
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(content[0], json!({"type": "text", "text": "describe"}));
    assert_eq!(
        content[1],
        json!({"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,Zm9v"}})
    );
    assert_eq!(
        content[2],
        json!({"type": "image_url", "image_url": {"url": "https://example.com/a.png"}})
    );
}

#[test]
fn openai_text_only_messages_keep_the_string_content() {
    let body = openai::build_body(
        &request(Message::user("plain")),
        false,
        MaxTokensField::MaxTokens,
    );
    assert_eq!(body["messages"][0]["content"], "plain");
}

#[test]
fn frames_are_labelled_with_their_timestamp() {
    let frames = [
        Frame::new(
            ImageSource::from_bytes("image/png", b"a"),
            Duration::from_millis(0),
        ),
        Frame::new(
            ImageSource::from_bytes("image/png", b"b"),
            Duration::from_millis(2500),
        ),
    ];
    let message = Message::user_with_frames("decode the motion", &frames);
    assert_eq!(message.content.len(), 5);
    assert_eq!(message.content[1], ContentBlock::text("Frame at 0.000 s:"));
    assert_eq!(message.content[3], ContentBlock::text("Frame at 2.500 s:"));
    assert!(request(message).has_images());
    assert!(!request(Message::user("x")).has_images());
}

#[test]
fn a_bad_image_fails_before_anything_is_sent() {
    let server = MockServer::start(|_, _| MockResponse::text(500, "must not be reached"));
    let anthropic = AnthropicProvider::new(&server.base_url(), client(), creds("anthropic"));
    let openai = OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai"));
    let bad_type = request(Message::user_blocks(vec![ContentBlock::image_bytes(
        "image/bmp",
        b"x",
    )]));
    let bad_url = request(Message::user_blocks(vec![ContentBlock::image_url(
        "file:///etc/passwd",
    )]));
    for req in [&bad_type, &bad_url] {
        assert!(matches!(
            anthropic.complete(req),
            Err(LlmError::InvalidRequest(_))
        ));
        assert!(matches!(
            openai.complete(req),
            Err(LlmError::InvalidRequest(_))
        ));
    }
    assert_eq!(server.request_count(), 0);
}

#[test]
fn an_image_in_an_assistant_message_is_rejected() {
    let server = MockServer::start(|_, _| MockResponse::text(500, "must not be reached"));
    let provider =
        OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai"));
    let mut assistant = Message::assistant("seen");
    assistant
        .content
        .push(ContentBlock::image_bytes("image/png", b"x"));
    let req = ChatRequest::new("m", vec![Message::user("hi"), assistant]);
    assert!(matches!(
        provider.complete(&req),
        Err(LlmError::InvalidRequest(_))
    ));
    assert_eq!(server.request_count(), 0);
}

#[test]
fn image_blocks_round_trip_through_serde() {
    let block = ContentBlock::image_bytes("image/webp", b"hello");
    let json = serde_json::to_string(&block).unwrap();
    assert_eq!(serde_json::from_str::<ContentBlock>(&json).unwrap(), block);
}
