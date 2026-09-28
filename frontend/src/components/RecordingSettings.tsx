import React, { useState, useEffect } from 'react';
import { Switch } from '@/components/ui/switch';
import { FolderOpen, RefreshCw } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { DeviceSelection, SelectedDevices } from '@/components/DeviceSelection';
import Analytics from '@/lib/analytics';
import { toast } from 'sonner';
import { useRecordingState } from '@/contexts/RecordingStateContext';
import { useConfig } from '@/contexts/ConfigContext';

export interface RecordingPreferences {
  save_folder: string;
  auto_save: boolean;
  file_format: string;
  preferred_mic_device: string | null;
  preferred_system_device: string | null;
}

interface RecordingSettingsProps {
  onSave?: (preferences: RecordingPreferences) => void;
}

export function RecordingSettings({ onSave }: RecordingSettingsProps) {
  const [preferences, setPreferences] = useState<RecordingPreferences>({
    save_folder: '',
    auto_save: true,
    file_format: 'ogg',
    preferred_mic_device: null,
    preferred_system_device: null
  });
  const [effectiveFolder, setEffectiveFolder] = useState<string>('');
  const [defaultFolder, setDefaultFolder] = useState<string>('');
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [showRecordingNotification, setShowRecordingNotification] = useState(true);
  const { isRecording } = useRecordingState();
  const { setSelectedDevices } = useConfig();

  // Load recording preferences on component mount
  useEffect(() => {
    const loadPreferences = async () => {
      try {
        const prefs = await invoke<RecordingPreferences>('get_recording_preferences');
        setPreferences(prefs);
      } catch (error) {
        console.error('Failed to load recording preferences:', error);
      }

      try {
        const defPath = await invoke<string>('get_default_recordings_folder_path');
        setDefaultFolder(defPath);
      } catch (defaultError) {
        console.error('Failed to get default folder path:', defaultError);
      }

      try {
        const effPath = await invoke<string>('get_effective_recordings_folder');
        setEffectiveFolder(effPath);
      } catch (effError) {
        console.error('Failed to get effective folder path:', effError);
      } finally {
        setLoading(false);
      }
    };

    loadPreferences();
  }, []);

  // Listen for fallback event if custom recording folder becomes inaccessible
  useEffect(() => {
    let unlistenFn: (() => void) | undefined;
    const setupListener = async () => {
      try {
        const { listen } = await import('@tauri-apps/api/event');
        unlistenFn = await listen<{ message: string; default_folder: string }>(
          'recording-folder-fallback',
          (event) => {
            toast.warning('Custom Save Folder Inaccessible', {
              description: event.payload.message,
            });
            setEffectiveFolder(event.payload.default_folder);
          }
        );
      } catch (err) {
        console.error('Failed to setup fallback event listener:', err);
      }
    };
    setupListener();
    return () => {
      unlistenFn?.();
    };
  }, []);

  // Load recording notification preference
  useEffect(() => {
    const loadNotificationPref = async () => {
      try {
        const { Store } = await import('@tauri-apps/plugin-store');
        const store = await Store.load('preferences.json');
        const show = await store.get<boolean>('show_recording_notification') ?? true;
        setShowRecordingNotification(show);
      } catch (error) {
        console.error('Failed to load notification preference:', error);
      }
    };
    loadNotificationPref();
  }, []);

  const handleAutoSaveToggle = async (enabled: boolean) => {
    const newPreferences = { ...preferences, auto_save: enabled };
    setPreferences(newPreferences);
    await savePreferences(newPreferences);

    // Track auto-save setting change
    await Analytics.track('auto_save_recording_toggled', {
      enabled: enabled.toString()
    });
  };

  const handleDeviceChange = async (devices: SelectedDevices) => {
    const newPreferences = {
      ...preferences,
      preferred_mic_device: devices.micDevice,
      preferred_system_device: devices.systemDevice
    };
    setPreferences(newPreferences);
    // Sync the in-memory selection ConfigContext exposes to the start path.
    // Without this, the picker only writes to disk (loaded into context once at
    // app mount), so a newly-chosen mic isn't honored until the next launch —
    // start keeps sending the stale launch-time device.
    setSelectedDevices(devices);
    await savePreferences(newPreferences);

    // Track default device preference changes
    // Note: Individual device selection analytics are tracked in DeviceSelection component
    await Analytics.track('default_devices_changed', {
      has_preferred_microphone: (!!devices.micDevice).toString(),
      has_preferred_system_audio: (!!devices.systemDevice).toString()
    });
  };

  const handleOpenFolder = async () => {
    try {
      await invoke('open_recordings_folder');
    } catch (error) {
      console.error('Failed to open recordings folder:', error);
    }
  };

  const handleBrowseFolder = async () => {
    try {
      const selected = await invoke<string | null>('select_recording_folder');
      if (selected) {
        setPreferences(prev => ({ ...prev, save_folder: selected }));
        const effPath = await invoke<string>('get_effective_recordings_folder');
        setEffectiveFolder(effPath);
        toast.success('Recording save folder updated', {
          description: selected,
        });
      }
    } catch (error) {
      console.error('Failed to select folder:', error);
      toast.error('Failed to select folder');
    }
  };

  const handleResetFolder = async () => {
    try {
      const defPath = await invoke<string>('reset_recording_folder_to_default');
      setPreferences(prev => ({ ...prev, save_folder: defPath }));
      setEffectiveFolder(defPath);
      toast.success('Reset save folder to default', {
        description: defPath,
      });
    } catch (error) {
      console.error('Failed to reset folder:', error);
      toast.error('Failed to reset folder');
    }
  };

  const handleNotificationToggle = async (enabled: boolean) => {
    try {
      setShowRecordingNotification(enabled);
      const { Store } = await import('@tauri-apps/plugin-store');
      const store = await Store.load('preferences.json');
      await store.set('show_recording_notification', enabled);
      await store.save();
      toast.success('Preference saved');
      await Analytics.track('recording_notification_preference_changed', {
        enabled: enabled.toString()
      });
    } catch (error) {
      console.error('Failed to save notification preference:', error);
      toast.error('Failed to save preference');
    }
  };

  const savePreferences = async (prefs: RecordingPreferences) => {
    setSaving(true);
    try {
      await invoke('set_recording_preferences', { preferences: prefs });
      onSave?.(prefs);

      // Show success toast with device details
      const micDevice = prefs.preferred_mic_device || 'Default';
      const systemDevice = prefs.preferred_system_device || 'Default';
      toast.success("Device preferences saved", {
        description: `Microphone: ${micDevice}, System Audio: ${systemDevice}`
      });
    } catch (error) {
      console.error('Failed to save recording preferences:', error);
      toast.error("Failed to save device preferences", {
        description: error instanceof Error ? error.message : String(error)
      });
    } finally {
      setSaving(false);
    }
  };

  if (loading) {
    return (
      <div className="animate-pulse">
        <div className="h-4 bg-gray-200 rounded w-1/4 mb-4"></div>
        <div className="h-8 bg-gray-200 rounded mb-4"></div>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <div>
        <h3 className="text-lg font-semibold mb-4">Recording Settings</h3>
        <p className="text-sm text-gray-600 mb-6">
          Configure how your audio recordings are saved during meetings.
        </p>
      </div>

      {/* Auto Save Toggle */}
      <div className="flex items-center justify-between p-4 border rounded-lg">
        <div className="flex-1">
          <div className="font-medium">Save Audio Recordings</div>
          <div className="text-sm text-gray-600">
            Automatically save audio files when recording stops
          </div>
        </div>
        <Switch
          checked={preferences.auto_save}
          onCheckedChange={handleAutoSaveToggle}
          disabled={saving}
        />
      </div>

      {/* Folder Location */}
      <div className="space-y-4">
        <div className="p-4 border rounded-lg bg-gray-50">
          <div className="font-medium mb-1">Save Location</div>
          <p className="text-xs text-gray-500 mb-3">
            Each meeting saves to its own subfolder (<code className="bg-gray-200/70 px-1 py-0.5 rounded text-xs font-mono">YYYY-MM-DD_Title</code>) containing audio, transcripts, and summary JSON.
          </p>
          <div className="text-sm font-mono text-gray-800 bg-white border border-gray-200 rounded p-2.5 mb-3 break-all select-all">
            {effectiveFolder || preferences.save_folder || 'Default folder'}
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <button
              onClick={handleBrowseFolder}
              disabled={isRecording}
              className="flex items-center gap-2 px-3 py-2 text-sm bg-white border border-gray-300 rounded-md hover:bg-gray-100 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            >
              <FolderOpen className="w-4 h-4 text-blue-600" />
              Browse...
            </button>
            <button
              onClick={handleOpenFolder}
              className="flex items-center gap-2 px-3 py-2 text-sm bg-white border border-gray-300 rounded-md hover:bg-gray-100 transition-colors"
            >
              <FolderOpen className="w-4 h-4 text-gray-600" />
              Open Folder
            </button>
            {defaultFolder && effectiveFolder && effectiveFolder !== defaultFolder && (
              <button
                onClick={handleResetFolder}
                disabled={isRecording}
                className="flex items-center gap-2 px-3 py-2 text-sm text-gray-600 hover:text-gray-900 border border-transparent hover:border-gray-200 rounded-md transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              >
                <RefreshCw className="w-4 h-4" />
                Reset to Default
              </button>
            )}
          </div>
        </div>

        {preferences.auto_save ? (
          <div className="p-4 border rounded-lg bg-blue-50">
            <div className="text-sm text-blue-800">
              <strong>File Format:</strong> {preferences.file_format.toUpperCase()} files
            </div>
            <div className="text-xs text-blue-600 mt-1">
              Audio recordings are saved as <code className="font-mono text-xs">audio.{preferences.file_format}</code> inside the meeting folder alongside transcript and <code className="font-mono text-xs">summary.json</code>.
            </div>
          </div>
        ) : (
          <div className="p-4 border rounded-lg bg-yellow-50">
            <div className="text-sm text-yellow-800">
              Audio recording auto-save is disabled. Transcripts and meeting summaries will still be saved to the folder above.
            </div>
          </div>
        )}
      </div>

      {/* Recording Notification Toggle */}
      <div className="flex items-center justify-between p-4 border rounded-lg">
        <div className="flex-1">
          <div className="font-medium">Recording Start Notification</div>
          <div className="text-sm text-gray-600">
            Show reminder to inform participants when recording starts
          </div>
        </div>
        <Switch
          checked={showRecordingNotification}
          onCheckedChange={handleNotificationToggle}
        />
      </div>

      {/* Device Preferences */}
      <div className="space-y-4">
        <div className="border-t pt-6">
          <h4 className="text-base font-medium text-gray-900 mb-4">Default Audio Devices</h4>
          <p className="text-sm text-gray-600 mb-4">
            Set your preferred microphone and system audio devices for recording. These will be automatically selected when starting new recordings.
          </p>

          {isRecording && (
            <p
              role="status"
              aria-live="polite"
              className="text-sm text-amber-700 bg-amber-50 border border-amber-200 rounded-md p-2 mb-4"
            >
              Device selection is locked while a recording is in progress. Connecting a new device mid-recording will not switch to it. Stop the current meeting to change devices.
            </p>
          )}

          <div className="border rounded-lg p-4 bg-gray-50">
            <DeviceSelection
              selectedDevices={{
                micDevice: preferences.preferred_mic_device,
                systemDevice: preferences.preferred_system_device
              }}
              onDeviceChange={handleDeviceChange}
              disabled={saving || isRecording}
            />
          </div>
        </div>
      </div>
    </div>
  );
}