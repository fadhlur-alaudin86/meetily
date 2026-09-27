'use client';

import React, {
  createContext,
  useContext,
  useState,
  useEffect,
  useCallback,
  useMemo,
} from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { MeetingBackup } from '@/types';

/**
 * Single source of truth for per-meeting backup status.
 *
 * Solves the stale-badge bug: sidebar badges and settings stats previously
 * lived in two disconnected local states that only refreshed on mount or
 * restart. This provider fetches once, then stays current via:
 * 1. `backup-updated` events emitted by the Rust backend after every
 *    successful backup (manual, backup-all, background auto-backup).
 * 2. `backup-failed` events when a meeting could not be backed up.
 * 3. Re-fetch on window focus (picks up zip files deleted outside the app).
 */

export interface BackupStats {
  ok: number;
  partial: number;
  failed: number;
}

interface BackupStatusContextType {
  /** Map of meeting_id -> latest backup record. */
  statuses: Record<string, MeetingBackup>;
  /** Derived counters over the status map. */
  stats: BackupStats;
  isLoading: boolean;
  /** Force a reconcile-on-fetch refresh (exposed for manual reloads). */
  refresh: () => Promise<void>;
}

const BackupStatusContext = createContext<BackupStatusContextType | null>(null);

export function useBackupStatus(): BackupStatusContextType {
  const context = useContext(BackupStatusContext);
  if (!context) {
    throw new Error('useBackupStatus must be used within a BackupStatusProvider');
  }
  return context;
}

export function BackupStatusProvider({ children }: { children: React.ReactNode }) {
  const [statuses, setStatuses] = useState<Record<string, MeetingBackup>>({});
  const [isLoading, setIsLoading] = useState(true);

  const refresh = useCallback(async () => {
    try {
      const list = await invoke<MeetingBackup[]>('api_get_all_backup_statuses');
      const map: Record<string, MeetingBackup> = {};
      for (const item of list) {
        map[item.meeting_id] = item;
      }
      setStatuses(map);
    } catch (error) {
      console.error('Failed to fetch backup statuses:', error);
    } finally {
      setIsLoading(false);
    }
  }, []);

  useEffect(() => {
    refresh();

    const unlisteners: UnlistenFn[] = [];
    let cleanedUp = false;

    const setupListeners = async () => {
      const unlistenUpdated = await listen<MeetingBackup>('backup-updated', (event) => {
        setStatuses((prev) => ({ ...prev, [event.payload.meeting_id]: event.payload }));
      });
      if (cleanedUp) {
        unlistenUpdated();
        return;
      }
      unlisteners.push(unlistenUpdated);

      const unlistenFailed = await listen<{ meeting_id: string; error: string }>(
        'backup-failed',
        (event) => {
          setStatuses((prev) => {
            const existing = prev[event.payload.meeting_id];
            return {
              ...prev,
              [event.payload.meeting_id]: {
                meeting_id: event.payload.meeting_id,
                backup_path: existing?.backup_path ?? '',
                status: 'failed',
                backed_up_at: existing?.backed_up_at ?? new Date().toISOString(),
                has_audio: existing?.has_audio ?? false,
                has_summary: existing?.has_summary ?? false,
              },
            };
          });
        }
      );
      if (cleanedUp) {
        unlistenFailed();
        return;
      }
      unlisteners.push(unlistenFailed);
    };

    setupListeners();

    // Re-fetch when the window regains focus: catches external changes such
    // as a backup zip deleted in the OS file manager (reconcile-on-fetch).
    const handleFocus = () => {
      refresh();
    };
    window.addEventListener('focus', handleFocus);

    return () => {
      cleanedUp = true;
      unlisteners.forEach((unlisten) => unlisten());
      window.removeEventListener('focus', handleFocus);
    };
  }, [refresh]);

  const stats = useMemo(() => {
    const counts: BackupStats = { ok: 0, partial: 0, failed: 0 };
    for (const backup of Object.values(statuses)) {
      if (backup.status === 'ok') counts.ok++;
      else if (backup.status === 'partial') counts.partial++;
      else if (backup.status === 'failed') counts.failed++;
    }
    return counts;
  }, [statuses]);

  const value = useMemo(
    () => ({ statuses, stats, isLoading, refresh }),
    [statuses, stats, isLoading, refresh]
  );

  return (
    <BackupStatusContext.Provider value={value}>{children}</BackupStatusContext.Provider>
  );
}
