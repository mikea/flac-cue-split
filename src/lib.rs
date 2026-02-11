pub type Result<T> = std::result::Result<T, String>;

mod app;
mod cli;
mod cue;
mod flac;
mod metadata;
mod output;
mod picture;
mod types;

pub use app::run;

#[cfg(test)]
mod tests;
