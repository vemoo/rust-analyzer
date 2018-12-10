use languageserver_types::{
    Location, Position, Range, SymbolKind, TextDocumentEdit, TextDocumentIdentifier,
    TextDocumentItem, TextDocumentPositionParams, TextEdit, Url, VersionedTextDocumentIdentifier,
};
use ra_analysis::{FileId, FileSystemEdit, SourceChange, SourceFileNodeEdit, FilePosition};
use ra_editor::{AtomEdit, Edit, LineCol, LineIndex};
use ra_syntax::{SyntaxKind, TextRange, TextUnit};

use crate::{req, server_world::ServerWorld, Result};

pub trait Conv {
    type Output;
    fn conv(self) -> Self::Output;
}

pub trait ConvWith {
    type Ctx;
    type Output;
    fn conv_with(self, ctx: &Self::Ctx) -> Self::Output;
}

pub trait TryConvWith {
    type Ctx;
    type Output;
    fn try_conv_with(self, ctx: &Self::Ctx) -> Result<Self::Output>;
}

impl Conv for SyntaxKind {
    type Output = SymbolKind;

    fn conv(self) -> <Self as Conv>::Output {
        match self {
            SyntaxKind::FN_DEF => SymbolKind::Function,
            SyntaxKind::STRUCT_DEF => SymbolKind::Struct,
            SyntaxKind::ENUM_DEF => SymbolKind::Enum,
            SyntaxKind::TRAIT_DEF => SymbolKind::Interface,
            SyntaxKind::MODULE => SymbolKind::Module,
            SyntaxKind::TYPE_DEF => SymbolKind::TypeParameter,
            SyntaxKind::STATIC_DEF => SymbolKind::Constant,
            SyntaxKind::CONST_DEF => SymbolKind::Constant,
            SyntaxKind::IMPL_ITEM => SymbolKind::Object,
            _ => SymbolKind::Variable,
        }
    }
}

impl ConvWith for Position {
    type Ctx = LineIndex;
    type Output = TextUnit;

    fn conv_with(self, line_index: &LineIndex) -> TextUnit {
        let line_col = LineCol {
            line: self.line as u32,
            col_utf16: self.character as u32,
        };
        line_index.offset(line_col)
    }
}

impl ConvWith for TextUnit {
    type Ctx = LineIndex;
    type Output = Position;

    fn conv_with(self, line_index: &LineIndex) -> Position {
        let line_col = line_index.line_col(self);
        Position::new(u64::from(line_col.line), u64::from(line_col.col_utf16))
    }
}

impl ConvWith for TextRange {
    type Ctx = LineIndex;
    type Output = Range;

    fn conv_with(self, line_index: &LineIndex) -> Range {
        Range::new(
            self.start().conv_with(line_index),
            self.end().conv_with(line_index),
        )
    }
}

impl ConvWith for Range {
    type Ctx = LineIndex;
    type Output = TextRange;

    fn conv_with(self, line_index: &LineIndex) -> TextRange {
        TextRange::from_to(
            self.start.conv_with(line_index),
            self.end.conv_with(line_index),
        )
    }
}

impl ConvWith for Edit {
    type Ctx = LineIndex;
    type Output = Vec<TextEdit>;

    fn conv_with(self, line_index: &LineIndex) -> Vec<TextEdit> {
        self.into_atoms()
            .into_iter()
            .map_conv_with(line_index)
            .collect()
    }
}

impl ConvWith for AtomEdit {
    type Ctx = LineIndex;
    type Output = TextEdit;

    fn conv_with(self, line_index: &LineIndex) -> TextEdit {
        TextEdit {
            range: self.delete.conv_with(line_index),
            new_text: self.insert,
        }
    }
}

impl<T: ConvWith> ConvWith for Option<T> {
    type Ctx = <T as ConvWith>::Ctx;
    type Output = Option<<T as ConvWith>::Output>;
    fn conv_with(self, ctx: &Self::Ctx) -> Self::Output {
        self.map(|x| ConvWith::conv_with(x, ctx))
    }
}

impl<'a> TryConvWith for &'a Url {
    type Ctx = ServerWorld;
    type Output = FileId;
    fn try_conv_with(self, world: &ServerWorld) -> Result<FileId> {
        world.uri_to_file_id(self)
    }
}

impl TryConvWith for FileId {
    type Ctx = ServerWorld;
    type Output = Url;
    fn try_conv_with(self, world: &ServerWorld) -> Result<Url> {
        world.file_id_to_uri(self)
    }
}

impl<'a> TryConvWith for &'a TextDocumentItem {
    type Ctx = ServerWorld;
    type Output = FileId;
    fn try_conv_with(self, world: &ServerWorld) -> Result<FileId> {
        self.uri.try_conv_with(world)
    }
}

impl<'a> TryConvWith for &'a VersionedTextDocumentIdentifier {
    type Ctx = ServerWorld;
    type Output = FileId;
    fn try_conv_with(self, world: &ServerWorld) -> Result<FileId> {
        self.uri.try_conv_with(world)
    }
}

impl<'a> TryConvWith for &'a TextDocumentIdentifier {
    type Ctx = ServerWorld;
    type Output = FileId;
    fn try_conv_with(self, world: &ServerWorld) -> Result<FileId> {
        world.uri_to_file_id(&self.uri)
    }
}

impl<'a> TryConvWith for &'a TextDocumentPositionParams {
    type Ctx = ServerWorld;
    type Output = FilePosition;
    fn try_conv_with(self, world: &ServerWorld) -> Result<FilePosition> {
        let file_id = self.text_document.try_conv_with(world)?;
        let line_index = world.analysis().file_line_index(file_id);
        let offset = self.position.conv_with(&line_index);
        Ok(FilePosition { file_id, offset })
    }
}

impl<T: TryConvWith> TryConvWith for Vec<T> {
    type Ctx = <T as TryConvWith>::Ctx;
    type Output = Vec<<T as TryConvWith>::Output>;
    fn try_conv_with(self, ctx: &Self::Ctx) -> Result<Self::Output> {
        let mut res = Vec::with_capacity(self.len());
        for item in self {
            res.push(item.try_conv_with(ctx)?);
        }
        Ok(res)
    }
}

impl TryConvWith for SourceChange {
    type Ctx = ServerWorld;
    type Output = req::SourceChange;
    fn try_conv_with(self, world: &ServerWorld) -> Result<req::SourceChange> {
        let cursor_position = match self.cursor_position {
            None => None,
            Some(pos) => {
                let line_index = world.analysis().file_line_index(pos.file_id);
                let edits = self
                    .source_file_edits
                    .iter()
                    .find(|it| it.file_id == pos.file_id)
                    .map(|it| it.edits.as_slice())
                    .unwrap_or(&[]);
                let line_col = translate_offset_with_edit(&*line_index, pos.offset, edits);
                let position =
                    Position::new(u64::from(line_col.line), u64::from(line_col.col_utf16));
                Some(TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier::new(pos.file_id.try_conv_with(world)?),
                    position,
                })
            }
        };
        let source_file_edits = self.source_file_edits.try_conv_with(world)?;
        let file_system_edits = self.file_system_edits.try_conv_with(world)?;
        Ok(req::SourceChange {
            label: self.label,
            source_file_edits,
            file_system_edits,
            cursor_position,
        })
    }
}

// HACK: we should translate offset to line/column using linde_index *with edits applied*.
// A naive version of this function would be to apply `edits` to the original text,
// construct a new line index and use that, but it would be slow.
//
// Writing fast & correct version is issue #105, let's use a quick hack in the meantime
fn translate_offset_with_edit(
    pre_edit_index: &LineIndex,
    offset: TextUnit,
    edits: &[AtomEdit],
) -> LineCol {
    let fallback = pre_edit_index.line_col(offset);
    let edit = match edits.first() {
        None => return fallback,
        Some(edit) => edit,
    };
    let end_offset = edit.delete.start() + TextUnit::of_str(&edit.insert);
    if !(edit.delete.start() <= offset && offset <= end_offset) {
        return fallback;
    }
    let rel_offset = offset - edit.delete.start();
    let in_edit_line_col = LineIndex::new(&edit.insert).line_col(rel_offset);
    let edit_line_col = pre_edit_index.line_col(edit.delete.start());
    if in_edit_line_col.line == 0 {
        LineCol {
            line: edit_line_col.line,
            col_utf16: edit_line_col.col_utf16 + in_edit_line_col.col_utf16,
        }
    } else {
        LineCol {
            line: edit_line_col.line + in_edit_line_col.line,
            col_utf16: in_edit_line_col.col_utf16,
        }
    }
}

impl TryConvWith for SourceFileNodeEdit {
    type Ctx = ServerWorld;
    type Output = TextDocumentEdit;
    fn try_conv_with(self, world: &ServerWorld) -> Result<TextDocumentEdit> {
        let text_document = VersionedTextDocumentIdentifier {
            uri: self.file_id.try_conv_with(world)?,
            version: None,
        };
        let line_index = world.analysis().file_line_index(self.file_id);
        let edits = self.edits.into_iter().map_conv_with(&line_index).collect();
        Ok(TextDocumentEdit {
            text_document,
            edits,
        })
    }
}

impl TryConvWith for FileSystemEdit {
    type Ctx = ServerWorld;
    type Output = req::FileSystemEdit;
    fn try_conv_with(self, world: &ServerWorld) -> Result<req::FileSystemEdit> {
        let res = match self {
            FileSystemEdit::CreateFile { anchor, path } => {
                let uri = world.file_id_to_uri(anchor)?;
                let path = &path.as_str()[3..]; // strip `../` b/c url is weird
                let uri = uri.join(path)?;
                req::FileSystemEdit::CreateFile { uri }
            }
            FileSystemEdit::MoveFile { file, path } => {
                let src = world.file_id_to_uri(file)?;
                let path = &path.as_str()[3..]; // strip `../` b/c url is weird
                let dst = src.join(path)?;
                req::FileSystemEdit::MoveFile { src, dst }
            }
        };
        Ok(res)
    }
}

pub fn to_location(
    file_id: FileId,
    range: TextRange,
    world: &ServerWorld,
    line_index: &LineIndex,
) -> Result<Location> {
    let url = file_id.try_conv_with(world)?;
    let loc = Location::new(url, range.conv_with(line_index));
    Ok(loc)
}

pub trait MapConvWith<'a>: Sized + 'a {
    type Ctx;
    type Output;

    fn map_conv_with(self, ctx: &'a Self::Ctx) -> ConvWithIter<'a, Self, Self::Ctx> {
        ConvWithIter { iter: self, ctx }
    }
}

impl<'a, I> MapConvWith<'a> for I
where
    I: Iterator + 'a,
    I::Item: ConvWith,
{
    type Ctx = <I::Item as ConvWith>::Ctx;
    type Output = <I::Item as ConvWith>::Output;
}

pub struct ConvWithIter<'a, I, Ctx: 'a> {
    iter: I,
    ctx: &'a Ctx,
}

impl<'a, I, Ctx> Iterator for ConvWithIter<'a, I, Ctx>
where
    I: Iterator,
    I::Item: ConvWith<Ctx = Ctx>,
{
    type Item = <I::Item as ConvWith>::Output;

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next().map(|item| item.conv_with(self.ctx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::{proptest, proptest_helper, string::RegexGeneratorStrategy};
    use proptest::prelude::*;

    fn arb_text() -> RegexGeneratorStrategy<std::string::String> {
        // generate multiple newlines
        proptest::string::string_regex("(.*\n?)*").unwrap()
    }

    fn arb_line_index_with_offset_and_edits() -> BoxedStrategy<(LineIndex, TextUnit, Vec<AtomEdit>)>
    {
        arb_text()
            .prop_flat_map(|s| {
                let line_index = LineIndex::new(&s);
                let char_indices: Vec<_> = s.char_indices().map(|(i, _)| i).collect();
                let arb_offset = arb_offset(char_indices);
                (Just(line_index), arb_offset.clone(), arb_edits(arb_offset))
            })
            .boxed()
    }

    fn arb_offset(char_indices: Vec<usize>) -> BoxedStrategy<TextUnit> {
        // this is necesary to avoid "Uniform::new called with `low >= high`" panic
        if char_indices.is_empty() {
            Just(TextUnit::from(0)).boxed()
        } else {
            prop::sample::select(char_indices)
                .prop_map(TextUnit::from_usize)
                .boxed()
        }
    }

    fn arb_edits(offsets: BoxedStrategy<TextUnit>) -> BoxedStrategy<Vec<AtomEdit>> {
        let ranges = (offsets.clone(), offsets.clone()).prop_map(|(x, y)| {
            let (from, to) = if x < y { (x, y) } else { (y, x) };
            TextRange::from_to(from, to)
        });
        let deletes = ranges.clone().prop_map(AtomEdit::delete).boxed();
        let inserts = (offsets.clone(), arb_text())
            .prop_map(|(offset, text)| AtomEdit::insert(TextUnit::from(offset), text))
            .boxed();
        let replaces = (ranges, arb_text())
            .prop_map(|(range, text)| AtomEdit::replace(range, text))
            .boxed();

        let arb_edit = deletes.prop_union(inserts).or(replaces);
        prop::collection::vec(arb_edit, 0..5).boxed()
    }

    proptest! {
        #[test]
        fn test_translate_offset_with_edit((line_index, offset, edits) in arb_line_index_with_offset_and_edits()) {
            let line_col = translate_offset_with_edit(&line_index, offset, &edits);
            println!("{:?}", line_col);
        }
    }

}
