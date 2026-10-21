//! Complete ASCII aggregates and Latin-context suffix proofs. The original
//! machine still consumes actual paragraph separators; only real EOF permits sealing.

use super::*;

enum Phase {
    Prove,
    Append,
    SealFrames,
    SealRuns,
    Ready,
}

pub(super) enum Progress {
    Pending,
    Ready,
    Rejected,
    Resume(MarkdownScriptRun),
}

pub(super) struct Preparation {
    proof: Option<crate::markdown_document_buffer::FrozenPrintableAsciiSuffix>,
    run: Option<MarkdownScriptRun>,
    phase: Phase,
    eof: bool,
}

impl Preparation {
    /// Captures only exact whole-source printable-ASCII facts already present
    /// on this immutable root, avoiding a second scan or another text owner.
    pub(super) fn capture(source: &MarkdownLayoutTextSource) -> Result<Option<Self>> {
        let Some(has_letters) = source.printable_ascii_has_letters()? else {
            return Ok(None); // Unicode/control characters require the original SCX/bracket machine.
        };
        if source.byte_len() != source.utf16_len() {
            return Err(MarkdownScriptError::CorruptMetadata);
        } // ASCII has one byte/UTF-16 unit per scalar; inconsistent lengths cannot pass.
        let run = if source.byte_len() == 0 {
            None // Empty input seals its two actual owners without inventing a run or coverage.
        } else {
            let script = if has_letters {
                // Exact aggregates prove an ASCII letter exists. Its only specific script
                // comes from the same Unicode table;
                // other Common characters are wildcards. ASCII paired brackets introduce
                // neither another specific script nor a paragraph boundary.
                MarkdownScript(properties::get('a').primary)
            } else {
                MarkdownScript(unicode_data::COMMON)
            };
            Some(MarkdownScriptRun {
                start_byte: 0,
                end_byte: source.byte_len(),
                start_utf16: 0,
                end_utf16: source.utf16_len(),
                script,
            })
        };
        source.health()?; // Even a no-read aggregate path preserves the original source's late I/O failure.
        Ok(Some(Self {
            proof: None,
            run,
            phase: Phase::Append,
            eof: true,
        }))
    }

    /// After the original machine establishes Latin with no pending bracket context,
    /// proves this run's suffix through the same source's actual content endpoint.
    pub(super) fn suffix(
        source: &MarkdownLayoutTextSource,
        mut run: MarkdownScriptRun,
        content_end: usize,
    ) -> Result<Self> {
        let proof = source.begin_printable_ascii_suffix(run.start_utf16..content_end)?;
        // Byte/UTF-16 coordinates coincide only after an ASCII proof. This candidate
        // byte endpoint grants no source lease while the proof remains unknown.
        run.end_byte = run
            .start_byte
            .checked_add(content_end - run.start_utf16)
            .ok_or(MarkdownScriptError::CoordinateOverflow)?;
        run.end_utf16 = content_end;
        Ok(Self {
            proof: Some(proof),
            run: Some(run),
            phase: Phase::Prove,
            eof: content_end == source.utf16_len(),
        })
    }

    /// Exposes this candidate's coordinate envelope only; the range itself
    /// grants neither a byte lease nor permission to publish an unfinished proof.
    pub(super) fn range(&self) -> Option<std::ops::Range<usize>> {
        self.run.map(|run| run.start_utf16..run.end_utf16)
    }

    /// Shares the original quantum's remaining byte/scalar/metadata budgets.
    /// Store phases advance only after the corresponding real operation succeeds.
    pub(super) fn advance(
        &mut self,
        source: &MarkdownLayoutTextSource,
        maximum_borrow: &mut usize,
        store: &mut store::Store,
        step: &mut MarkdownScriptStep,
        budget: MarkdownScriptBudget,
    ) -> Result<Progress> {
        loop {
            match self.phase {
                Phase::Prove => {
                    let actual = source.advance_printable_ascii_suffix(
                        self.proof.as_mut().expect("原后缀证明处于Prove阶段"),
                        budget.bytes - step.bytes,
                        budget.scalars - step.scalars,
                        budget.stack_ops - step.stack_ops,
                    )?;
                    step.bytes += actual.bytes;
                    step.scalars += actual.scalars;
                    step.stack_ops += actual.metadata;
                    *maximum_borrow = (*maximum_borrow).max(actual.maximum_borrow);
                    match actual.answer {
                        None => return Ok(Progress::Pending),
                        // A real counterexample returns control to the untouched SCX
                        // machine; it cannot publish a shortened or guessed ASCII run.
                        Some(false) => return Ok(Progress::Rejected),
                        Some(true) => {
                            self.proof = None;
                            if !self.eof {
                                return Ok(Progress::Resume(
                                    self.run.expect("非EOF证明保原当前run"),
                                ));
                            } // Before a real paragraph separator, resume the original machine rather than append a closed run.
                            self.phase = Phase::Append;
                        }
                    } // A counterexample changes no machine/run; only the complete proven suffix permits sealing.
                }
                Phase::Append => {
                    if let Some(run) = self.run {
                        if step.records == budget.records {
                            return Ok(Progress::Pending); // Keep this unwritten run and phase for a later admitted quantum.
                        }
                        store.append_run(run)?;
                        step.records += 1;
                    }
                    self.phase = Phase::SealFrames;
                }
                Phase::SealFrames => {
                    if step.stack_ops == budget.stack_ops {
                        return Ok(Progress::Pending);
                    }
                    store.seal_frames()?; // Actually close bracket storage; a skipped scan is no release receipt.
                    step.stack_ops += 1;
                    self.phase = Phase::SealRuns;
                }
                Phase::SealRuns => {
                    if step.stack_ops == budget.stack_ops {
                        return Ok(Progress::Pending);
                    }
                    store.seal_runs()?; // Seal actual 64-byte records and return write storage before publishing Ready.
                    step.stack_ops += 1;
                    self.phase = Phase::Ready;
                }
                Phase::Ready => return Ok(Progress::Ready),
            }
        }
    }
}

#[cfg(test)]
mod tests;
