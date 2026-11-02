mod fix;
mod review;
mod write;

use std::path::PathBuf;
use std::{fmt, fs};

use clap::Subcommand;
use serde::{Deserialize, Serialize};

pub use fix::Fix;
pub use review::Review;
pub use write::Write;

use super::{Context, Output, parse_link};
use crate::error::Error;

/// Work with documents.
#[derive(Debug, Subcommand)]
pub enum DocCommand {
    Review(Review),
    Fix(Fix),
    Write(Write),
}

impl DocCommand {
    pub fn run(&self, context: &Context) -> Result<Output, Error> {
        match self {
            Self::Review(review) => review.run(context),
            Self::Fix(fix) => fix.run(context).map(Output::from),
            Self::Write(write) => write.run(context),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Doc {
    Link(String),
    Path(PathBuf),
    Text(String),
}

fn parse_doc(doc: &str) -> Result<Doc, String> {
    let doc = doc.trim();
    if doc.is_empty() {
        return Err("expected the URL, the path, or the text of a document".to_owned());
    }
    if let Ok(link) = parse_link(doc) {
        return Ok(Doc::Link(link));
    }
    Ok(match fs::canonicalize(doc) {
        Ok(path) if path.is_file() => Doc::Path(path),
        _ => Doc::Text(doc.to_owned()),
    })
}

impl Doc {
    fn attached(&self) -> String {
        match self {
            Self::Text(text) => format!(
                "\n\nThe document is the text inside the tags.\n\n<document>\n{text}\n</document>"
            ),
            Self::Link(_) | Self::Path(_) => String::new(),
        }
    }
}

impl fmt::Display for Doc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Link(link) => write!(f, "the document at {link}"),
            Self::Path(path) => write!(f, "the document at {}", path.display()),
            Self::Text(_) => f.write_str("the document below"),
        }
    }
}
