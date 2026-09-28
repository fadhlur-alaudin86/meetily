"use client";

import {
  forwardRef,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
} from "react";
import type { Block } from "@blocknote/core";
import { useCreateBlockNote } from "@blocknote/react";
import { BlockNoteView } from "@blocknote/shadcn";
import { blocksToMarkdownSafely } from "@/lib/blocknote-markdown";
import "@blocknote/shadcn/style.css";
import "@blocknote/core/fonts/inter.css";

export interface NotesSavePayload {
  /** Markdown rendering of the document; `null` when conversion failed. */
  markdown: string | null;
  /** Serialized BlockNote document (JSON string for notes_json). */
  notes_json: string;
}

export interface NotesEditorRef {
  /** Converts the current document into the save payload. */
  save: () => Promise<NotesSavePayload>;
  /** Marks the current content as saved (clears the dirty flag). */
  resetDirty: () => void;
}

interface NotesEditorProps {
  /** BlockNote blocks from `notes_json`, when present. */
  initialBlocks?: Block[] | null;
  /** Markdown fallback used when `notes_json` is absent or unparsable. */
  initialMarkdown?: string | null;
  onDirtyChange?: (isDirty: boolean) => void;
}

/**
 * Standalone BlockNote editor for meeting notes.
 *
 * Mounted only after the parent has fetched data (never during SSR), which
 * keeps the notes page compatible with Next.js static export.
 */
export const NotesEditor = forwardRef<NotesEditorRef, NotesEditorProps>(
  ({ initialBlocks, initialMarkdown, onDirtyChange }, ref) => {
    const editor = useCreateBlockNote({
      initialContent: initialBlocks ?? undefined,
    });
    const isLoadedRef = useRef(false);
    const [isDirty, setIsDirty] = useState(false);

    // Load markdown content when notes_json was absent, then start tracking
    // edits only after the initial content has settled (mirrors the
    // isContentLoaded guard in BlockNoteSummaryView).
    useEffect(() => {
      let cancelled = false;

      const markLoaded = () => {
        window.setTimeout(() => {
          if (!cancelled) isLoadedRef.current = true;
        }, 100);
      };

      const load = async () => {
        if (!initialBlocks && initialMarkdown) {
          try {
            // `await` keeps this correct whether the parser is sync or async.
            const blocks = await editor.tryParseMarkdownToBlocks(initialMarkdown);
            if (!cancelled) editor.replaceBlocks(editor.document, blocks);
          } catch (err) {
            console.error("Failed to parse notes markdown:", err);
          }
        }
        markLoaded();
      };
      load();

      return () => {
        cancelled = true;
      };
    }, [editor, initialBlocks, initialMarkdown]);

    useEffect(() => {
      onDirtyChange?.(isDirty);
    }, [isDirty, onDirtyChange]);

    useImperativeHandle(
      ref,
      () => ({
        save: async () => {
          const markdownResult = await blocksToMarkdownSafely(
            editor,
            editor.document,
            { source: "NotesEditor.save" },
          );
          return {
            markdown:
              markdownResult.markdown !== undefined
                ? markdownResult.markdown
                : null,
            notes_json: JSON.stringify(editor.document),
          };
        },
        resetDirty: () => setIsDirty(false),
      }),
      [editor],
    );

    return (
      <BlockNoteView
        editor={editor}
        editable
        theme="light"
        onChange={() => {
          if (isLoadedRef.current) setIsDirty(true);
        }}
      />
    );
  },
);

NotesEditor.displayName = "NotesEditor";
