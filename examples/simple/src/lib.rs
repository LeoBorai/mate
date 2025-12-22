use mate_job::{mate_handler, mate_record};

#[mate_record]
struct UserInput {
    pub name: String,
}

#[mate_handler]
fn execute(input: UserInput) -> String {
    format!("Name is {}", input.name)
}
