"use client";

import { Suspense, useCallback, useEffect, useRef, useState } from "react";
import { useRouter, useSearchParams } from "next/navigation";
import { ArrowLeft, LoaderIcon, Save } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { toast } from "sonner";
import type { Block } from "@blocknote/core";
import { Button } from "@/components/ui/button";
import { MeetingNotes } from "@/types";
import {
  NotesEditor,
  NotesEditorRef,
} from "@/components/MeetingNotes/NotesEditor";

interface MeetingMetadata {
  id: string;
  title: string;
  created_at: string;
  updated_at: string;
}

interface NotesInitialContent {
  blocks: Block[] | null;
  markdown: string | null;
}

function formatDate(value: string): string {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
}

function parseNotesBlocks(notesJson: string | null | undefined): Block[] | null {
  if (!notesJson) return null;
  try {
    const parsed = JSON.parse(notesJson);
    if (Array.isArray(parsed) && parsed.length > 0) {
      return parsed as Block[];
    }
  } catch (err) {
    console.error("Failed to parse notes_json, falling back to markdown:", err);
  }
  return null;
}

function NotesContent() {
  const router = useRouter();
  const searchParams = useSearchParams();
  const meetingId = searchParams.get("id");

  const editorRef = useRef<NotesEditorRef>(null);
  const [meeting, setMeeting] = useState<MeetingMetadata | null>(null);
  const [notes, setNotes] = useState<MeetingNotes | null>(null);
  // Set exactly once when loading finishes; keeps the editor from re-seeding
  // its content after every save.
  const [initialContent, setInitialContent] = useState<NotesInitialContent>({
    blocks: null,
    markdown: null,
  });
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [isDirty, setIsDirty] = useState(false);
  const [isSaving, setIsSaving] = useState(false);

  useEffect(() => {
    if (!meetingId) {
      setLoadError("No meeting selected.");
      setLoading(false);
      return;
    }

    let cancelled = false;

    const load = async () => {
      try {
        const metadata = await invoke<MeetingMetadata>(
          "api_get_meeting_metadata",
          { meetingId },
        );
        if (cancelled) return;
        setMeeting(metadata);
      } catch (err) {
        console.error("Failed to load meeting for notes:", err);
        if (!cancelled) setLoadError(String(err));
        if (!cancelled) setLoading(false);
        return;
      }

      try {
        const row = await invoke<MeetingNotes | null>("api_get_meeting_notes", {
          meetingId,
        });
        if (cancelled) return;
        setNotes(row ?? null);
        setInitialContent({
          blocks: parseNotesBlocks(row?.notes_json),
          markdown: row?.notes_markdown ?? null,
        });
      } catch (err) {
        // Keep the page usable with an empty editor.
        console.error("Failed to load notes:", err);
        if (cancelled) return;
        setInitialContent({ blocks: null, markdown: null });
      }
      if (!cancelled) setLoading(false);
    };

    load();
    return () => {
      cancelled = true;
    };
  }, [meetingId]);

  const handleSave = useCallback(async () => {
    if (!meetingId || !editorRef.current || isSaving || !isDirty) return;

    setIsSaving(true);
    try {
      const payload = await editorRef.current.save();
      const saved = await invoke<MeetingNotes | null>("api_save_meeting_notes", {
        meetingId,
        notesMarkdown: payload.markdown,
        notesJson: payload.notes_json,
      });
      setNotes(saved);
      editorRef.current?.resetDirty();
      if (saved) {
        toast.success("Notes saved");
      } else {
        toast.success("Notes cleared");
      }
    } catch (err) {
      console.error("Failed to save notes:", err);
      toast.error("Failed to save notes", { description: String(err) });
    } finally {
      setIsSaving(false);
    }
  }, [meetingId, isDirty, isSaving]);

  // Ctrl/Cmd+S saves the note (Q7: explicit save, no navigation warning).
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "s") {
        event.preventDefault();
        handleSave();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [handleSave]);

  if (loading) {
    return (
      <div className="flex items-center justify-center h-screen">
        <LoaderIcon className="animate-spin size-6" />
      </div>
    );
  }

  if (loadError || !meeting) {
    return (
      <div className="flex flex-col items-center justify-center h-screen gap-4">
        <p className="text-gray-600">{loadError ?? "Meeting not found."}</p>
        <Button
          variant="outline"
          size="sm"
          onClick={() => router.push(meeting ? `/meeting-details?id=${meeting.id}` : "/")}
        >
          <ArrowLeft />
          Back
        </Button>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-screen bg-white">
      <div className="flex items-center gap-3 border-b border-gray-200 px-6 py-4">
        <Button
          variant="outline"
          size="sm"
          onClick={() => router.push(`/meeting-details?id=${meeting.id}`)}
          title="Back to meeting"
        >
          <ArrowLeft />
          <span className="hidden sm:inline">Back to meeting</span>
        </Button>

        <div className="min-w-0 flex-1">
          <h1 className="text-lg font-semibold truncate">{meeting.title}</h1>
          <p className="text-xs text-gray-500">
            {formatDate(meeting.created_at)}
            {notes && <span> · Last edited {formatDate(notes.updated_at)}</span>}
            {isDirty && (
              <span className="text-amber-600"> · Unsaved changes</span>
            )}
          </p>
        </div>

        <Button
          size="sm"
          onClick={handleSave}
          disabled={!isDirty || isSaving}
          title="Save notes (Ctrl+S)"
        >
          <Save />
          {isSaving ? "Saving..." : "Save"}
        </Button>
      </div>

      <div className="flex-1 overflow-y-auto min-h-0">
        <div className="mx-auto w-full max-w-4xl p-6">
          <NotesEditor
            ref={editorRef}
            initialBlocks={initialContent.blocks}
            initialMarkdown={initialContent.markdown}
            onDirtyChange={setIsDirty}
          />
        </div>
      </div>
    </div>
  );
}

export default function NotesPage() {
  return (
    <Suspense
      fallback={
        <div className="flex items-center justify-center h-screen">
          <LoaderIcon className="animate-spin size-6" />
        </div>
      }
    >
      <NotesContent />
    </Suspense>
  );
}
