use std::path::PathBuf;
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct Diff {
    pub source_dir: PathBuf,
    pub target_delta_dir: PathBuf,
}

#[derive(Debug, StructOpt)]
pub struct Apply {
    pub source_dir: PathBuf,
    pub delta_target_dir: PathBuf,
}

#[derive(Debug, StructOpt)]
pub enum DockerFile {
    Diff {
        image_a: String,
        image_b: String,

        #[structopt(long)]
        override_version: Option<String>,
    },
    Apply {
        delta_image: String,

        #[structopt(long)]
        override_version: Option<String>,
    },
}

#[derive(Debug, StructOpt)]
pub enum Command {
    Diff(Diff),
    Apply(Apply),
    DockerFile(DockerFile)
}

#[derive(StructOpt, Debug)]
pub struct Cmdline {
    #[structopt(long, short="d")]
    pub debug: bool,

    #[structopt(subcommand)]
    pub command: Command,
}
