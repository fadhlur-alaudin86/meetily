import React, { useState, useEffect, useCallback } from 'react';
import { Switch } from '@/components/ui/switch';
import { FolderOpen, RefreshCw, Archive, CheckCircle2, Clock, AlertCircle } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'sonner';
import { BackupPreferences, MeetingBackup } from '@/types';

export function BackupSettings() {
  const [preferences, setPreferences] = useState<BackupPreferences>({
    backup_folder: '',
    auto_backup: true,
  });
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [backingUpAll, setBackingUpAll] = useState(false);
  const [backupStats, setBackupStats] = useState<{ ok: number; partial: number; failed: number }>({
    ok: 0,
    partial: 0,
    failed: 0,
  });

  const loadPreferences = useCallback(async () => {
    try {
      const prefs = await invoke<BackupPreferences>('api_get_backup_preferences');
      setPreferences(prefs);
    } catch (error) {
      console.error('Failed to load backup preferences:', error);
      toast.error('Failed to load backup preferences');
    } finally {
      setLoading(false);
    }
  }, []);

  const loadBackupStats = useCallback(async () => {
    try {
      const statuses = await invoke<MeetingBackup[]>('api_get_all_backup_statuses');
      const stats = { ok: 0, partial: 0, failed: 0 };
      for (const item of statuses) {
        if (item.status === 'ok') stats.ok++;
        else if (item.status === 'partial') stats.partial++;
        else if (item.status === 'failed') stats.failed++;
      }
      setBackupStats(stats);
    } catch (error) {
      console.error('Failed to load backup statistics:', error);
    }
  }, []);

  useEffect(() => {
    loadPreferences();
    loadBackupStats();
  }, [loadPreferences, loadBackupStats]);

  const handleAutoBackupToggle = async (enabled: boolean) => {
    const updated = { ...preferences, auto_backup: enabled };
    setPreferences(updated);
    setSaving(true);
    try {
      await invoke('api_save_backup_preferences', { preferences: updated });
      toast.success(enabled ? 'Auto-backup enabled' : 'Auto-backup disabled');
    } catch (error) {
      console.error('Failed to update auto-backup setting:', error);
      toast.error('Failed to update backup preference');
    } finally {
      setSaving(false);
    }
  };

  const handleBrowseFolder = async () => {
    try {
      const selected = await invoke<string | null>('api_select_backup_folder');
      if (selected) {
        setPreferences(prev => ({ ...prev, backup_folder: selected }));
        toast.success('Backup folder updated', { description: selected });
      }
    } catch (error) {
      console.error('Failed to select backup folder:', error);
      toast.error('Failed to select folder');
    }
  };

  const handleOpenFolder = async () => {
    try {
      await invoke('api_open_backup_folder');
    } catch (error) {
      console.error('Failed to open backup folder:', error);
      toast.error('Failed to open folder');
    }
  };

  const handleResetFolder = async () => {
    try {
      const defPath = await invoke<string>('api_reset_backup_folder_to_default');
      setPreferences(prev => ({ ...prev, backup_folder: defPath }));
      toast.success('Reset backup folder to default', { description: defPath });
    } catch (error) {
      console.error('Failed to reset backup folder:', error);
      toast.error('Failed to reset folder');
    }
  };

  const handleBackupAll = async () => {
    setBackingUpAll(true);
    try {
      const results = await invoke<MeetingBackup[]>('api_backup_all_meetings');
      toast.success('Backup complete', {
        description: `Successfully backed up ${results.length} meeting(s).`,
      });
      await loadBackupStats();
    } catch (error) {
      console.error('Failed to back up all meetings:', error);
      toast.error('Backup all failed', {
        description: error instanceof Error ? error.message : String(error),
      });
    } finally {
      setBackingUpAll(false);
    }
  };

  if (loading) {
    return (
      <div className="animate-pulse space-y-4">
        <div className="h-6 bg-gray-200 rounded w-1/4"></div>
        <div className="h-20 bg-gray-200 rounded"></div>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <div>
        <h3 className="text-lg font-semibold mb-1">Local Note Backup</h3>
        <p className="text-sm text-gray-600">
          Configure offline backup archives for your meetings, audio recordings, transcripts, and AI summaries.
        </p>
      </div>

      {/* Auto Backup Toggle */}
      <div className="flex items-center justify-between p-4 border rounded-lg">
        <div className="flex-1 mr-4">
          <div className="font-medium">Automatic Backup</div>
          <div className="text-sm text-gray-600">
            Automatically create or update a standalone <code className="text-xs bg-gray-100 px-1 py-0.5 rounded font-mono">.zip</code> archive whenever a new note is saved or an AI summary completes.
          </div>
        </div>
        <Switch
          checked={preferences.auto_backup}
          onCheckedChange={handleAutoBackupToggle}
          disabled={saving}
        />
      </div>

      {/* Backup Folder Location */}
      <div className="p-4 border rounded-lg bg-gray-50 space-y-3">
        <div>
          <div className="font-medium text-sm text-gray-900">Backup Directory</div>
          <p className="text-xs text-gray-500 mt-0.5">
            Individual meeting archives (<code className="text-xs bg-gray-200 px-1 py-0.5 rounded font-mono">YYYY-MM-DD_Title_backup.zip</code>) will be saved in this directory.
          </p>
        </div>
        <div className="text-sm font-mono text-gray-800 bg-white border border-gray-200 rounded p-2.5 break-all select-all">
          {preferences.backup_folder || 'Default: ~/Documents/meetily-backups'}
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <button
            onClick={handleBrowseFolder}
            className="flex items-center gap-1.5 px-3 py-1.5 text-sm bg-white border border-gray-300 rounded-md hover:bg-gray-100 transition-colors"
          >
            <FolderOpen className="w-4 h-4 text-blue-600" />
            Browse...
          </button>
          <button
            onClick={handleOpenFolder}
            className="flex items-center gap-1.5 px-3 py-1.5 text-sm bg-white border border-gray-300 rounded-md hover:bg-gray-100 transition-colors"
          >
            <FolderOpen className="w-4 h-4 text-gray-600" />
            Open Folder
          </button>
          <button
            onClick={handleResetFolder}
            className="flex items-center gap-1.5 px-3 py-1.5 text-sm text-gray-600 hover:text-gray-900 border border-transparent hover:border-gray-200 rounded-md transition-colors"
          >
            <RefreshCw className="w-4 h-4" />
            Reset to Default
          </button>
        </div>
      </div>

      {/* Backup Status Overview & Action */}
      <div className="p-4 border rounded-lg space-y-4">
        <div>
          <div className="font-medium text-sm text-gray-900">Backup Status Overview</div>
          <p className="text-xs text-gray-500 mt-0.5">
            Status check for all recorded meetings in your local database.
          </p>
        </div>

        <div className="grid grid-cols-3 gap-3">
          <div className="p-3 bg-green-50 border border-green-200 rounded-lg flex items-center space-x-3">
            <CheckCircle2 className="w-5 h-5 text-green-600 flex-shrink-0" />
            <div>
              <div className="text-lg font-bold text-green-800">{backupStats.ok}</div>
              <div className="text-xs text-green-700">Complete (OK)</div>
            </div>
          </div>
          <div className="p-3 bg-amber-50 border border-amber-200 rounded-lg flex items-center space-x-3">
            <Clock className="w-5 h-5 text-amber-600 flex-shrink-0" />
            <div>
              <div className="text-lg font-bold text-amber-800">{backupStats.partial}</div>
              <div className="text-xs text-amber-700">Partial (No Summary)</div>
            </div>
          </div>
          <div className="p-3 bg-red-50 border border-red-200 rounded-lg flex items-center space-x-3">
            <AlertCircle className="w-5 h-5 text-red-600 flex-shrink-0" />
            <div>
              <div className="text-lg font-bold text-red-800">{backupStats.failed}</div>
              <div className="text-xs text-red-700">Failed / Errors</div>
            </div>
          </div>
        </div>

        <div className="pt-2 flex items-center justify-between border-t border-gray-100">
          <div className="text-xs text-gray-500">
            Back up all existing meeting folders with a single click.
          </div>
          <button
            onClick={handleBackupAll}
            disabled={backingUpAll}
            className="flex items-center gap-2 px-4 py-2 text-sm font-medium text-white bg-blue-600 hover:bg-blue-700 rounded-md transition-colors disabled:opacity-50 disabled:cursor-not-allowed shadow-sm"
          >
            <Archive className="w-4 h-4" />
            {backingUpAll ? 'Backing up all...' : 'Backup All Meetings'}
          </button>
        </div>
      </div>
    </div>
  );
}
