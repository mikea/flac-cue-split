pub type Result<T> = std::result::Result<T, String>;

mod app;
mod cli;
mod cue;
mod decoder;
mod flac;
mod metadata;
mod output;
mod picture;
mod split;
mod types;
mod wavpack;

pub use app::run;

#[cfg(test)]
mod tests;
