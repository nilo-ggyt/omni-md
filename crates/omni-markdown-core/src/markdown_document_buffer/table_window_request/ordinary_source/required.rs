//! Resolve an actual neighboring row within the original source owner and batch, without a second Global or implied ordinary readiness.

use super::super::semantic::{RequiredLocation, RequiredPublication};
use super::*;
use crate::markdown_document_buffer::MarkdownTableViewportRequiredRowQuery;
use crate::markdown_viewport_presentation::table_window::navigation::MarkdownTableWindowNavigation;

/// Capture both publication identities at issuance; equal table coordinates cannot renew another source publication's authority.
pub struct MarkdownViewportSourceRequiredRowQuery {
    publication: Weak<SourcePublication>,
    table_query: MarkdownTableViewportRequiredRowQuery,
}

/// Return an original source row ticket for either a row or real End, rather than an index or integer that could manufacture authority.
pub enum MarkdownViewportSourceRequiredRowResult {
    Row {
        row: VerifiedMarkdownTableRowSource,
    },
    TableEnd {
        terminal: VerifiedMarkdownTableRowSource,
    },
}

impl VerifiedMarkdownViewportSources {
    /// Require an actual structural member of this publication; direction selects a target that still needs replay.
    /// None means Previous at the header. Next and NextRow request the next physical row; the cell owns column policy.
    pub fn required_row_query_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        origin: &VerifiedMarkdownTableRowSource,
        navigation: MarkdownTableWindowNavigation,
    ) -> Result<Option<MarkdownViewportSourceRequiredRowQuery>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        origin.require_for(document, authority)?;
        if !Rc::ptr_eq(&origin.publication, &self.publication) {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        let requested_row = match navigation {
            MarkdownTableWindowNavigation::Previous => match origin.ordinal.checked_sub(1) {
                Some(row) => row,
                None => return Ok(None),
            },
            MarkdownTableWindowNavigation::Next | MarkdownTableWindowNavigation::NextRow => origin
                .ordinal
                .checked_add(1)
                .ok_or(MarkdownTableRequestError::InvalidRequest)?,
        };
        let table_query = self.table.required_structure_query_for(
            document,
            &origin.ticket,
            origin.ordinal,
            requested_row,
        )?;
        Ok(Some(MarkdownViewportSourceRequiredRowQuery {
            publication: Rc::downgrade(&self.publication),
            table_query,
        }))
    }

    /// Expose only a completed replay's row or terminal ticket, even when the target lies outside visual coverage.
    pub fn required_row_result_for(
        &self,
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<Option<MarkdownViewportSourceRequiredRowResult>, MarkdownTableRequestError> {
        self.check(document, authority)?;
        let Some(result) = self.table.required_publication_for(document)? else {
            return Ok(None);
        };
        let (index, ordinal, ended) = match result {
            RequiredPublication::Row {
                location: RequiredLocation::Structure(index),
                row_ordinal,
            } => (*index, *row_ordinal, false),
            RequiredPublication::End {
                terminal: RequiredLocation::Structure(index),
                end,
            } => (*index, end.last_body_row_ordinal().unwrap_or(0), true),
            _ => return Err(MarkdownTableRequestError::InvalidRequest),
        };
        let ticket = self
            .table
            .structural_tables()
            .get(index)
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let row = VerifiedMarkdownTableRowSource {
            ticket: Rc::clone(ticket),
            publication: Rc::clone(&self.publication),
            ordinal,
            terminal: match result {
                RequiredPublication::End { end, .. } => Some(end.clone()),
                RequiredPublication::Row { .. } => ticket
                    .table_end
                    .as_ref()
                    .filter(|end| end.last_body_row_ordinal().unwrap_or(0) == ordinal)
                    .cloned(),
            },
        };
        row.require_for(document, authority)?;
        Ok(Some(if ended {
            MarkdownViewportSourceRequiredRowResult::TableEnd { terminal: row }
        } else {
            MarkdownViewportSourceRequiredRowResult::Row { row }
        }))
    }
}

impl MarkdownViewportSourceRequest {
    /// Add a neighboring-row requirement to the original batch; reject backward visual coverage or a missing original checkpoint.
    ///
    /// Success starts Pending work. Advance to SourcesAvailable before consuming the actual Row or End.
    pub fn continue_forward_with_required_row(
        &mut self,
        document: &MarkdownDocumentBuffer,
        previous: &VerifiedMarkdownViewportSources,
        viewport: Range<usize>,
        authority: MarkdownViewportSourceAuthority,
        query: MarkdownViewportSourceRequiredRowQuery,
    ) -> Result<(), MarkdownTableRequestError> {
        self.check_forward_sources(document, previous, &viewport, authority)?;
        if !query
            .publication
            .ptr_eq(&Rc::downgrade(&previous.publication))
        {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        // The inner batch independently checks its TablePublication and origin membership; source directory identity cannot replace either.
        match self.batch.extend_from_ready_with_required_row(
            document,
            &previous.table,
            viewport.clone(),
            query.table_query,
        ) {
            Ok(()) => {} // The batch accepted the query; coexisting ordinary metadata still needs joint admission below.
            Err(
                error @ (MarkdownTableRequestError::PublicationSuperseded
                | MarkdownTableRequestError::CoverageRequiresCheckpoint
                | MarkdownTableRequestError::InvalidRequest),
            ) => {
                // These query/coverage preconditions fail before the inner transition; the original owner can still accept a valid query.
                return Err(error);
            }
            Err(error) => {
                self.fail(error); // Quota or source failure retires this incomplete replay; exported results retain only their original read rights.
                return Err(error);
            }
        }
        // Admit the previous ordinary directory only after batch acceptance, using the same quota and cumulative work.
        // Retention preserves the job, coordinates, and origin_floor; unfinished ordinary work precedes its viewport successor.
        let result = self.retain_forward_sources(document, previous, viewport);
        if let Err(error) = result {
            self.fail(error); // Joint inventory failure retires this successor; another request must not silently reissue its budget.
        }
        result
    }
}

#[cfg(test)]
mod tests;
