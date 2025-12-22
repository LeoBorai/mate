use mate_job::{mate_handler, mate_record};

#[mate_record]
struct SendEmailJob {
    pub name: String,
    pub email: String,
    pub age: u32,
    pub tags: Vec<String>,
}

#[mate_record]
struct EmailOutput {
    pub html: String,
    pub success: bool,
    pub message_id: String,
}

#[mate_handler]
fn execute(input: SendEmailJob) -> EmailOutput {
    let html = format!(
        "<h1>Hello, {}!</h1><p>We are excited to have you on board.</p><p>Your age: {}</p><p>Tags: {}</p>",
        input.name,
        input.age,
        input.tags.join(", ")
    );

    EmailOutput {
        html,
        success: true,
        message_id: "mocked-message-id-123".to_string(),
    }
}
