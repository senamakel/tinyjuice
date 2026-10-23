use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn request(token: &str) -> GenerateRequest {
    GenerateRequest {
        context_token: token.to_owned(),
        purpose: "tool_output_summary".into(),
        system: "system".into(),
        prompt: "prompt".into(),
        max_output_tokens: 16,
    }
}

#[tokio::test]
async fn an_unknown_token_declines() {
    assert_eq!(serve(request("not-registered")).await, Ok(None));
}

#[tokio::test]
async fn a_registered_callback_receives_the_request_and_is_consumed_once() {
    let ticket = register(Box::new(|request| {
        Box::pin(async move { Ok(format!("{} / {}", request.system, request.prompt)) })
    }));
    let token = ticket.token().to_owned();
    assert_eq!(
        serve(request(&token)).await,
        Ok(Some("system / prompt".into()))
    );
    assert_eq!(serve(request(&token)).await, Ok(None));
}

#[tokio::test]
async fn simultaneous_requests_can_take_a_callback_only_once() {
    let calls = Arc::new(AtomicUsize::new(0));
    let called = Arc::clone(&calls);
    let ticket = register(Box::new(move |_| {
        Box::pin(async move {
            called.fetch_add(1, Ordering::SeqCst);
            Ok("summary".to_owned())
        })
    }));
    let request = request(ticket.token());
    let (left, right) = tokio::join!(serve(request.clone()), serve(request));
    assert_eq!(
        usize::from(matches!(left, Ok(Some(_)))) + usize::from(matches!(right, Ok(Some(_)))),
        1
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn dropping_a_ticket_withdraws_its_callback() {
    let ticket = register(Box::new(|_| {
        Box::pin(async { panic!("must not be called") })
    }));
    let token = ticket.token().to_owned();
    drop(ticket);
    assert_eq!(serve(request(&token)).await, Ok(None));
}

#[tokio::test]
async fn callback_errors_are_returned_to_the_module() {
    let ticket = register(Box::new(|_| {
        Box::pin(async { Err(anyhow::anyhow!("provider offline")) })
    }));
    assert_eq!(
        serve(request(ticket.token())).await,
        Err("provider offline".into())
    );
}
