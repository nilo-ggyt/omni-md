//! Streaming native saves, history-preserving autosave baselines, and collision-safe conflict copies.

use super::*;

impl MarkdownSessionAccess {
    /// Persists the native editor's piece tree without constructing a flat
    /// document string. Conflicts remain exclusively in the live document
    /// buffer; this path never creates a recovery draft, cache, or index.
    ///
    /// A successful commit updates the saved baseline while retaining the canonical
    /// mapping, edit segments and bounded undo/redo roots. Debounced autosave must
    /// not erase editing history. All session storage still releases on teardown.
    #[cfg(feature = "native-markdown-candidate")]
    /// Commit streamed native edits only when the authoritative disk revision still matches the save baseline.
    pub fn save_document_buffer(
        &self,
        document: &mut MarkdownDocumentBuffer,
    ) -> Result<MarkdownSaveReceipt, MarkdownWorkspaceError> {
        // Production always uses the real durability operation; no global hook or runtime flag exists.
        self.save_document_buffer_with_directory_sync(document, sync_directory)
    }

    /// Run one native save with a caller-owned single-use post-publication durability operation.
    ///
    /// The private seam permits deterministic failure after the actual rename in pure filesystem
    /// tests. Earlier acquisition/conflict paths never call it; public callers cannot replace it.
    #[cfg(feature = "native-markdown-candidate")]
    fn save_document_buffer_with_directory_sync(
        &self,
        document: &mut MarkdownDocumentBuffer,
        synchronize_directory: impl FnOnce(&Path) -> Result<(), MarkdownWorkspaceError>,
    ) -> Result<MarkdownSaveReceipt, MarkdownWorkspaceError> {
        // Reject an unpublished native input before touching disk. Discovering
        // a busy baseline only after overwrite would publish the wrong text.
        document
            .ensure_text_input_idle()
            .map_err(markdown_document_buffer_save_error)?;
        let expected_revision = document.original_revision().encode_hex();
        let relative_path =
            relative_markdown_path(&self.canonical_root, document.path()).map_err(|_| {
                // Unverifiable association or revision state cannot authorize replacing authoritative disk bytes.
                MarkdownWorkspaceError::SaveConflict
            })?;
        let path =
            resolve_existing_markdown(&self.canonical_root, &relative_path).map_err(|_| {
                // Unverifiable association or revision state cannot authorize replacing authoritative disk bytes.
                MarkdownWorkspaceError::SaveConflict
            })?;
        if path != document.path() {
            // A changed association cannot authorize overwriting another vault path.
            return Err(MarkdownWorkspaceError::SaveConflict);
        }

        // Compare exact content revisions before acquiring a replacement temporary.
        let current = inspect_markdown_save_baseline(&path)?;
        if current.revision != expected_revision {
            // A disk revision different from the opened baseline requires reload or an explicit conflict.
            if current.revision
                == document
                    .current_revision()
                    .map_err(markdown_document_buffer_save_error)?
                    .encode_hex()
            {
                // Identical live and external bytes can converge safely without overwriting either version.
                return adopt_matching_document_buffer_baseline(document, &path, &current.revision);
            }
            // Even a clean live buffer cannot silently change canonical characters.
            // Explicit UI reload owns replacement and the native graph/cache notifications;
            // this persistence operation reports only the conflicting disk observation.
            return Err(MarkdownWorkspaceError::SaveConflict);
        }
        if !document.is_dirty() {
            // A clean unchanged document needs no temporary file, rewrite, or history change.
            return Ok(MarkdownSaveReceipt {
                revision: current.revision,
                byte_count: current.byte_count,
                modified_at_ms: current.modified_at_ms,
            });
        }

        // Preserve the existing note's permissions and confine the sibling temporary to the same parent.
        let metadata = fs::symlink_metadata(&path).map_err(|_| {
            // Unverifiable association or revision state cannot authorize replacing authoritative disk bytes.
            MarkdownWorkspaceError::SaveConflict
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            // The destination must still be a regular file before acquiring replacement permissions.
            return Err(MarkdownWorkspaceError::SaveConflict);
        }
        let original_permissions = metadata.permissions();
        let parent = path
            .parent()
            .ok_or(MarkdownWorkspaceError::InvalidRelativePath)?;
        ensure_within_root(&self.canonical_root, parent)?;

        // Stream and sync complete bytes under rollback ownership before making a final revision check.
        let mut temporary = create_temporary_markdown_save(parent, &self.save_sequence)?;
        temporary
            .file
            .set_permissions(original_permissions)
            .map_err(|_| {
                // Translate a failed filesystem stage into the stable vault-storage error category.
                MarkdownWorkspaceError::StorageUnavailable
            })?;
        document
            .write_to(&mut temporary.file)
            .map_err(markdown_document_buffer_save_error)?;
        temporary.file.sync_all().map_err(|_| {
            // Translate a failed filesystem stage into the stable vault-storage error category.
            MarkdownWorkspaceError::StorageUnavailable
        })?;

        // Validate the complete streamed temporary through an independent immutable snapshot before publication.
        let mut replacement = MarkdownDocumentBuffer::open(&temporary.path, MAX_DOCUMENT_BYTES)
            .map_err(markdown_document_buffer_save_error)?;
        let saved_revision = replacement.original_revision().encode_hex();
        let rechecked = inspect_markdown_save_baseline(&path)?;
        if rechecked.revision != expected_revision {
            // A revision change while streaming invalidates the proposed overwrite.
            if rechecked.revision == saved_revision {
                // An external writer that already committed identical bytes can be adopted safely.
                return adopt_matching_document_buffer_baseline(
                    document,
                    &path,
                    &rechecked.revision,
                );
            }
            return Err(MarkdownWorkspaceError::SaveConflict);
        }

        // Atomic publication transfers the temporary name; history remains owned by the existing live buffer.
        fs::rename(&temporary.path, &path).map_err(|_| {
            // Translate a failed filesystem stage into the stable vault-storage error category.
            MarkdownWorkspaceError::StorageUnavailable
        })?;
        temporary.disarm();
        // Rename already published new bytes. Revoke old content-search authority
        // immediately even if later sync or metadata fails; the directory key stays.
        self.invalidate_content_search();
        replacement
            .adopt_atomic_save_destination(&path)
            .map_err(markdown_document_buffer_save_error)?;
        // Require directory durability before marking the buffer clean; failure leaves the live draft intact.
        synchronize_directory(parent)?;
        let committed_metadata = fs::symlink_metadata(&path).map_err(|_| {
            // Translate a failed filesystem stage into the stable vault-storage error category.
            MarkdownWorkspaceError::StorageUnavailable
        })?;
        if committed_metadata.file_type().is_symlink() || !committed_metadata.is_file() {
            // A substituted post-commit path cannot be reported as the saved regular note.
            return Err(MarkdownWorkspaceError::SaveConflict);
        }
        // Adopt only validated identical content and current identity, preserving every undo/redo root.
        document
            .adopt_saved_baseline(&replacement)
            .map_err(markdown_document_buffer_save_error)?;
        Ok(MarkdownSaveReceipt {
            revision: saved_revision,
            byte_count: document.byte_len() as u64,
            modified_at_ms: modified_at_ms(&committed_metadata),
        })
    }

    /// Save live draft pieces into a collision-safe new note without modifying the original.
    ///
    /// The draft remains dirty, retains its history/mapping, and stays associated with
    /// its original path. The UI explicitly chooses when to switch to the returned
    /// note. No full-document String, persistent recovery draft, or overwrite occurs.
    ///
    /// # Errors
    ///
    /// Reject invalid/outside/reserved destinations, unsafe source identity, exhausted
    /// names, and storage failures. Failed writes remove only the newly owned temporary
    /// file; the original live buffer and external authoritative file remain untouched.
    #[cfg(feature = "native-markdown-candidate")]
    pub fn save_document_buffer_as_new(
        &self,
        document: &MarkdownDocumentBuffer,
        parent_directory: &str,
        title: &str,
    ) -> Result<MarkdownNoteMutationReceipt, MarkdownWorkspaceError> {
        // Production save-as always performs real directory synchronization; no
        // runtime failure switch is exposed.
        self.save_document_buffer_as_new_with_directory_sync(
            document,
            parent_directory,
            title,
            sync_directory,
        )
    }

    /// Private durability boundary after save-as publication. Rollback and terminal
    /// authority revocation still own failures after the real rename.
    #[cfg(feature = "native-markdown-candidate")]
    pub(super) fn save_document_buffer_as_new_with_directory_sync(
        &self,
        document: &MarkdownDocumentBuffer,
        parent_directory: &str,
        title: &str,
        mut synchronize_directory: impl FnMut(&Path) -> Result<(), MarkdownWorkspaceError>,
    ) -> Result<MarkdownNoteMutationReceipt, MarkdownWorkspaceError> {
        // Save-as also waits for input publication, so the old canonical root is
        // never saved as though it were the candidate the user is committing.
        document
            .ensure_text_input_idle()
            .map_err(markdown_document_buffer_save_error)?;
        // Terminal invalidation follows pending-file rollback. Save-as is a
        // structure change, not an overwrite of the current note's content key.
        let publication = WorkspaceStructurePublication::new(self);
        let result = (|| {
            // Require a buffer originating in this vault without reopening a now-conflicted file.
            relative_markdown_path(&self.canonical_root, document.path())?;
            let parent = resolve_existing_directory(&self.canonical_root, parent_directory)?;
            let title = validated_note_title(title)?;

            // Pin the original parent before creating the temporary. After rename,
            // retain this identity rather than adopting a possibly replaced path.
            let parent_descriptor = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&parent)
                .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?;
            verify_attachment_parent(&self.canonical_root, &parent, &parent_descriptor)?;
            // Stream into an exclusively created sibling; Drop owns cleanup until publication.
            let mut temporary = create_temporary_markdown_save(&parent, &self.save_sequence)?;
            document
                .write_to(&mut temporary.file)
                .map_err(markdown_document_buffer_save_error)?;
            temporary.file.sync_all().map_err(|_| {
                // Translate a failed filesystem stage into the stable vault-storage error category.
                MarkdownWorkspaceError::StorageUnavailable
            })?;
            let revision = document
                .current_revision()
                .map_err(markdown_document_buffer_save_error)?
                .encode_hex();
            // Freeze acquisition identity before rename. Publication may seal complete
            // metadata but cannot change the original device/inode authority.
            let temporary_identity = FileIdentity::from(
                &temporary
                    .file
                    .metadata()
                    .map_err(|_| MarkdownWorkspaceError::StorageUnavailable)?,
            );

            // Atomic exclusive rename never exposes a partially written note or replaces an entry.
            for suffix in 1_u16..=1_000 {
                let name = if suffix == 1 {
                    // The requested title is preferred when no file or symlink already owns it.
                    format!("{title}.md")
                } else {
                    // A bounded suffix sequence keeps both the existing entry and the live draft.
                    format!("{title} {suffix}.md")
                };
                let destination = parent.join(name);
                match rename_without_replacement(&temporary.path, &destination) {
                    Ok(()) => {
                        // The final file now owns the bytes; the temporary-path cleanup is obsolete.
                        temporary.disarm();
                        // The new note name invalidates old query continuations. The pending
                        // guard remains responsible for cleanup on subsequent failure.
                        publication.changed();
                        // Seal post-rename ctime from the original descriptor. If cloning
                        // that descriptor fails, never reacquire a replacement by path.
                        let acquired_destination = temporary
                            .file
                            .try_clone()
                            .and_then(|file| {
                                // Clone only the acquired descriptor, then bind its
                                // complete post-rename value to the original parent owner.
                                AcquiredMarkdownFile::seal(
                                    file,
                                    temporary_identity,
                                    document.byte_len() as u64,
                                    parent_descriptor,
                                    parent.clone(),
                                    &self.canonical_root,
                                )
                            })
                            .map_err(|_| {
                                // Publication already happened, but cleanup ownership
                                // could not be sealed. Retain the visible name and report
                                // the original storage failure with rollback uncertainty.
                                publication.rollback_unconfirmed();
                                MarkdownWorkspaceError::StorageUnavailable
                            })?;
                        let mut pending = PendingVaultFile::new(
                            destination.clone(),
                            acquired_destination,
                            &publication,
                        );
                        synchronize_directory(&parent)?;
                        let receipt =
                            mutation_receipt(&self.canonical_root, &destination, revision)?;
                        // Recheck original descriptor, path, and parent after directory sync.
                        // An external replacement cannot borrow that sync success as a
                        // receipt for a different saved file.
                        pending.verify_for_commit()?;
                        pending.disarm();
                        return Ok(receipt);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        // A collision leaves the temporary draft intact for the next unused name.
                    }
                    Err(_) => {
                        // Temporary Drop cleans failed publication without touching the original note.
                        return Err(MarkdownWorkspaceError::StorageUnavailable);
                    }
                }
            }
            // Exhaustion drops the unpublished temporary while preserving every preexisting entry.
            Err(MarkdownWorkspaceError::NameCollisionLimitExceeded)
        })();
        // Finish pending-file destruction before terminal invalidation, preserving
        // any external modification and the original save failure.
        publication.finish(result)
    }
}

#[cfg(test)]
mod tests {
    //! Conflict copies keep authoritative external bytes and the unsaved live draft independent.

    use super::*;
    use crate::MarkdownTextSelection;

    /// Saving a new note is streaming and exclusive, without changing the conflicted buffer.
    #[test]
    fn save_as_new_preserves_external_original_live_history_and_name_collisions() {
        // The source/destination files are synthetic and confined to a temporary vault.
        let directory = tempfile::tempdir().expect("temporary vault directory");
        let root = directory.path().join("vault");
        let workspace = MarkdownWorkspace::new(root.clone());
        let access = workspace.open_session().expect("open synthetic vault");
        fs::write(root.join("Note.md"), "before").expect("seed original");
        fs::write(root.join("Draft.md"), "existing destination").expect("seed collision");
        let mut document = access
            .open_document_buffer("Note.md")
            .expect("open original");
        let draft = "unsaved 中文 😀\r\n".repeat(8_000);
        document
            .replace_utf16_range(0..6, &draft, MarkdownTextSelection::caret(0))
            .expect("retain draft only in live edit segments");
        fs::write(root.join("Note.md"), "external").expect("external authoritative update");

        // The normal save detects the conflict and must not choose a winner automatically.
        assert_eq!(
            access.save_document_buffer(&mut document).err(),
            Some(MarkdownWorkspaceError::SaveConflict)
        );
        let revision = document.current_revision().unwrap();
        let original_path = document.path().to_path_buf();
        let selection = document.selection();
        let generation = document.generation();
        let receipt = access
            .save_document_buffer_as_new(&document, "", "Draft")
            .expect("save draft as independent note");

        // Exclusive publication preserves both old files while streaming all unsaved draft bytes.
        assert_eq!(receipt.relative_path, "Draft 2.md");
        assert_eq!(fs::read_to_string(root.join("Draft 2.md")).unwrap(), draft);
        assert_eq!(
            fs::read_to_string(root.join("Draft.md")).unwrap(),
            "existing destination"
        );
        assert_eq!(
            fs::read_to_string(root.join("Note.md")).unwrap(),
            "external"
        );
        assert_eq!(receipt.byte_count, draft.len() as u64);
        assert_eq!(receipt.revision, revision.encode_hex());

        // The UI, not persistence, owns the later switch; the old draft/history stay unchanged.
        assert_eq!(document.path(), original_path);
        assert_eq!(document.current_revision().unwrap(), revision);
        assert_eq!(document.selection(), selection);
        assert_eq!(document.generation(), generation);
        assert!(document.is_dirty());
        assert!(document.can_undo());
        assert!(fs::read_dir(&root).unwrap().all(|entry| {
            // Successful publication leaves no generated save file alongside user notes.
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".omni-md-save-")
        }));
    }

    /// Invalid destinations fail before acquiring a temporary file or changing the live draft.
    #[test]
    fn save_as_new_rejects_outside_and_reserved_destinations() {
        // Make one clean source whose bytes must survive every rejected destination.
        let directory = tempfile::tempdir().expect("temporary vault directory");
        let root = directory.path().join("vault");
        let workspace = MarkdownWorkspace::new(root.clone());
        let access = workspace.open_session().expect("open synthetic vault");
        fs::write(root.join("Note.md"), "source").expect("seed original");
        fs::create_dir(root.join("attachments")).expect("reserved attachment directory");
        let document = access
            .open_document_buffer("Note.md")
            .expect("open original");
        for destination in ["../", "attachments"] {
            // Neither traversal nor the reserved attachment tree may become a new note parent.
            assert!(
                access
                    .save_document_buffer_as_new(&document, destination, "Draft")
                    .is_err()
            );
        }
        assert!(
            access
                .save_document_buffer_as_new(&document, "", "../escape")
                .is_err()
        );
        assert_eq!(fs::read_to_string(root.join("Note.md")).unwrap(), "source");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
    }
    /// Debounced or explicit save must preserve both undo and redo across repeated filesystem commits.
    #[test]
    fn saves_preserve_undo_redo_and_saved_root_dirty_comparison() {
        // Keep all note, snapshot and save resources within one isolated synthetic vault.
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("vault");
        let workspace = MarkdownWorkspace::new(root.clone());
        let access = workspace.open_session().unwrap();
        fs::write(root.join("Note.md"), "before").unwrap();
        let mut document = access.open_document_buffer("Note.md").unwrap();
        let before = crate::MarkdownTextSelection::caret(2);
        document.set_selection(before).unwrap();
        let after = crate::MarkdownTextSelection {
            anchor_utf16: 0,
            head_utf16: 7,
        };
        document
            .replace_utf16_range_with_selection(0..6, "saved😀", before, after)
            .unwrap();
        let generation = document.generation();
        let profile = document.storage_profile();

        // A save changes only the persistence baseline; it cannot alter selection or input generation.
        access.save_document_buffer(&mut document).unwrap();
        assert!(!document.is_dirty());
        assert!(document.can_undo());
        assert_eq!(document.selection(), after);
        assert_eq!(document.generation(), generation);
        assert_eq!(
            document.storage_profile().appended_edit_bytes,
            profile.appended_edit_bytes
        );
        assert_eq!(document.storage_profile().undo_entries, 1);
        assert!(document.original_file_identity_is_unchanged());

        // Undo changes live text without changing already committed disk bytes; redo returns to clean.
        assert_eq!(document.undo(), Some(before));
        assert!(document.is_dirty());
        assert_eq!(fs::read_to_string(root.join("Note.md")).unwrap(), "saved😀");
        assert_eq!(document.redo(), Some(after));
        assert!(!document.is_dirty());

        // Autosaving an undone state must retain redo, then a later redo becomes a new dirty draft.
        document.undo().unwrap();
        access.save_document_buffer(&mut document).unwrap();
        assert!(!document.is_dirty());
        assert!(document.can_redo());
        assert_eq!(fs::read_to_string(root.join("Note.md")).unwrap(), "before");
        assert_eq!(document.redo(), Some(after));
        assert!(document.is_dirty());
        access.save_document_buffer(&mut document).unwrap();
        assert!(!document.is_dirty());
        assert!(document.can_undo());
        assert_eq!(
            document.storage_profile().appended_edit_bytes,
            profile.appended_edit_bytes
        );
    }

    /// Identical bytes saved externally converge without silently resetting the user's draft history.
    #[test]
    fn identical_external_revision_adoption_keeps_history_and_detects_next_conflict() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("vault");
        let workspace = MarkdownWorkspace::new(root.clone());
        let access = workspace.open_session().unwrap();
        fs::write(root.join("Note.md"), "before").unwrap();
        let mut document = access.open_document_buffer("Note.md").unwrap();
        document
            .replace_utf16_range(0..6, "mine", crate::MarkdownTextSelection::caret(4))
            .unwrap();
        // A separate writer happens to commit the same content, making convergence safe without rewrite.
        fs::write(root.join("Note.md"), "mine").unwrap();
        let generation = document.generation();
        access.save_document_buffer(&mut document).unwrap();
        assert!(!document.is_dirty());
        assert!(document.can_undo());
        assert_eq!(document.generation(), generation);
        assert_eq!(
            document.undo(),
            Some(crate::MarkdownTextSelection::caret(0))
        );
        assert!(document.is_dirty());

        // A subsequent independent external edit remains protected by the newly adopted baseline.
        fs::write(root.join("Note.md"), "external next").unwrap();
        assert_eq!(
            access.save_document_buffer(&mut document),
            Err(MarkdownWorkspaceError::SaveConflict)
        );
        assert_eq!(
            fs::read_to_string(root.join("Note.md")).unwrap(),
            "external next"
        );
        let mut live = Vec::new();
        document.write_to(&mut live).unwrap();
        assert_eq!(live, b"before");
        assert!(document.can_redo());
    }

    /// An injected post-rename sync failure preserves live history, then public retry adopts identical bytes.
    #[test]
    fn post_rename_durability_failure_retries_without_losing_history() {
        use std::cell::Cell;
        use std::os::unix::fs::MetadataExt;

        // The fixture has both undo and redo before the save, independently of native UI ownership.
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("vault");
        let workspace = MarkdownWorkspace::new(root.clone());
        let access = workspace.open_session().unwrap();
        let path = root.join("Note.md");
        fs::write(&path, "before").unwrap();
        let mut document = access.open_document_buffer("Note.md").unwrap();
        let draft = "draft😀";
        let end = draft.encode_utf16().count();
        document
            .replace_utf16_range(0..6, draft, crate::MarkdownTextSelection::caret(end))
            .unwrap();
        document
            .replace_utf16_range(end..end, "!", crate::MarkdownTextSelection::caret(end + 1))
            .unwrap();
        document.undo_text_edit().unwrap();
        let profile = document.storage_profile();
        let original_revision = document.original_revision();
        let current_revision = document.current_revision().unwrap();
        let generation = document.generation();
        let selection = document.selection();
        assert!(document.can_undo());
        assert!(document.can_redo());
        let called = Cell::new(false);

        // Hold a directory continuation and content-search authority before real
        // publication. A later durability failure must not restore stale content
        // search while the unchanged directory continuation remains valid.
        let mut pager = access.open_workspace_pager().unwrap();
        let super::paging::MarkdownWorkspacePageProgress::Ready(browse_page) = pager
            .next_page_cancellable(
                12,
                512,
                &std::collections::HashSet::new(),
                &std::sync::atomic::AtomicBool::new(false),
            )
            .unwrap()
        else {
            panic!("单笔记fixture必须一步完整扫描");
        };
        let search_page = access
            .search_notes_page_cancellable(
                "before",
                1,
                None,
                &std::sync::atomic::AtomicBool::new(false),
            )
            .unwrap();

        // Fail only the one post-publication durability stage, after the real file rename completed.
        let failure = access.save_document_buffer_with_directory_sync(&mut document, |parent| {
            called.set(true);
            assert_eq!(parent, root.canonicalize().unwrap());
            assert_eq!(fs::read_to_string(&path).unwrap(), draft);
            // Content-search revocation must follow the real rename immediately,
            // before this deliberate directory-sync failure is returned.
            assert!(!search_page.authority.is_current(&access, "before"));
            assert!(
                browse_page
                    .authority
                    .is_current(&access, &std::collections::HashSet::new())
            );
            assert!(fs::read_dir(parent).unwrap().all(|entry| {
                // Rename already consumed the sibling name; no unpublished temporary survives this point.
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".omni-md-save-")
            }));
            Err(MarkdownWorkspaceError::StorageUnavailable)
        });
        assert!(called.get());
        assert_eq!(failure, Err(MarkdownWorkspaceError::StorageUnavailable));
        assert!(!search_page.authority.is_current(&access, "before"));
        assert!(
            browse_page
                .authority
                .is_current(&access, &std::collections::HashSet::new())
        );
        let published_inode = fs::metadata(&path).unwrap().ino();
        // A physically published file is not a clean-buffer acknowledgement when directory sync failed.
        assert!(document.is_dirty());
        assert_eq!(document.original_revision(), original_revision);
        assert_eq!(document.current_revision().unwrap(), current_revision);
        assert_eq!(document.storage_profile(), profile);
        assert_eq!(document.generation(), generation);
        assert_eq!(document.selection(), selection);
        assert!(!document.original_file_identity_is_unchanged());

        // The unchanged public API uses real directory sync and identical-byte convergence on retry.
        let receipt = access.save_document_buffer(&mut document).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().ino(), published_inode);
        assert_eq!(receipt.revision, current_revision.encode_hex());
        assert_eq!(receipt.byte_count, draft.len() as u64);
        assert_eq!(document.original_revision(), current_revision);
        assert!(document.original_file_identity_is_unchanged());
        assert!(!document.is_dirty());
        assert_eq!(document.storage_profile(), profile);
        assert_eq!(document.generation(), generation);
        assert_eq!(document.selection(), selection);
        // Both history directions survive retry; returning to the saved root restores clean state exactly.
        assert_eq!(
            document.redo(),
            Some(crate::MarkdownTextSelection::caret(end + 1))
        );
        assert!(document.is_dirty());
        assert_eq!(document.undo(), Some(selection));
        assert!(!document.is_dirty());
        assert_eq!(
            document.undo(),
            Some(crate::MarkdownTextSelection::caret(0))
        );
        assert!(document.is_dirty());
        assert_eq!(fs::read_to_string(&path).unwrap(), draft);
        assert!(fs::read_dir(&root).unwrap().all(|entry| {
            // Neither the injected failure nor the retry leaves generated save files in the vault.
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".omni-md-save-")
        }));
    }
}
