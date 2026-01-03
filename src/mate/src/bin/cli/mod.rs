mod cmd;

use clap::Parser;

use self::cmd::Cmd;

#[derive(Debug, Parser)]
#[command(
    name = "mate",
    author = "Leo Borai <estebanborai@gmail.com>",
    about = "🦀🧡🧉"
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

impl Cli {
    pub async fn exec(&self) -> anyhow::Result<()> {
        match &self.cmd {
            Cmd::Task(task_cmd) => task_cmd.exec().await,
        }
    }
}
