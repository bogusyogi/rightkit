//! Minimal Typst `World`: one detached main source, explicit fonts, no files, no packages.

use crate::{FontSource, TypesetError};
use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::syntax::{FileId, Source};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};
use typst_layout::PagedDocument;

/// The font set shared by every compile.
pub(crate) struct FontSet {
    library: LazyHash<Library>,
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
}

impl FontSet {
    pub(crate) fn load(source: FontSource) -> Result<Self, TypesetError> {
        let mut fonts: Vec<Font> = Vec::new();
        match source {
            FontSource::None => {}
            FontSource::Bytes(files) => {
                let had_input = !files.is_empty();
                for data in files {
                    fonts.extend(Font::iter(Bytes::new(data)));
                }
                if had_input && fonts.is_empty() {
                    return Err(TypesetError::UnreadableFonts);
                }
            }
            #[cfg(feature = "embedded-fonts")]
            FontSource::Embedded => {
                for data in typst_assets::fonts() {
                    fonts.extend(Font::iter(Bytes::new(data)));
                }
            }
        }
        Ok(Self {
            library: LazyHash::new(Library::builder().build()),
            book: LazyHash::new(FontBook::from_fonts(&fonts)),
            fonts,
        })
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.fonts.is_empty()
    }

    pub(crate) fn compile(&self, text: String) -> Result<PagedDocument, TypesetError> {
        let world = Detached {
            set: self,
            main: Source::detached(text),
        };
        typst::compile::<PagedDocument>(&world)
            .output
            .map_err(|errs| {
                TypesetError::Compile(
                    errs.iter()
                        .map(|d| d.message.to_string())
                        .collect::<Vec<_>>()
                        .join("; "),
                )
            })
    }
}

struct Detached<'a> {
    set: &'a FontSet,
    main: Source,
}

impl World for Detached<'_> {
    fn library(&self) -> &LazyHash<Library> {
        &self.set.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        &self.set.book
    }

    fn main(&self) -> FileId {
        self.main.id()
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.main.id() {
            Ok(self.main.clone())
        } else {
            Err(FileError::AccessDenied)
        }
    }

    fn file(&self, _id: FileId) -> FileResult<Bytes> {
        Err(FileError::AccessDenied)
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.set.fonts.get(index).cloned()
    }

    fn today(&self, _offset: Option<Duration>) -> Option<Datetime> {
        None
    }
}
