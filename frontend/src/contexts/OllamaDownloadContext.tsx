'use client';

import React, { createContext, useContext, useState, useEffect, useRef } from 'react';
import { listen } from '@tauri-apps/api/event';
import { toast } from 'sonner';

/**
 * Ollama download state synchronized with backend
 * This context provides persistent download state that survives component unmounts,
 * solving:
 * 1. Lost progress when modal closes
 * 2. Duplicate download requests
 * 3. No feedback for background downloads
 */

interface OllamaDownloadState {
  downloadProgress: Map<string, number>;  // modelName -> progress (0-100)
  downloadingModels: Set<string>;         // Set of model names currently downloading
}

interface OllamaDownloadContextType extends OllamaDownloadState {
  isDownloading: (modelName: string) => boolean;
  getProgress: (modelName: string) => number | undefined;
}

const OllamaDownloadContext = createContext<OllamaDownloadContextType | null>(null);

export const useOllamaDownload = () => {
  const context = useContext(OllamaDownloadContext);
  if (!context) {
    throw new Error('useOllamaDownload must be used within an OllamaDownloadProvider');
  }
  return context;
};

export function OllamaDownloadProvider({ children }: { children: React.ReactNode }) {
  const [downloadProgress, setDownloadProgress] = useState<Map<string, number>>(new Map());
  const [downloadingModels, setDownloadingModels] = useState<Set<string>>(new Set());

  // Throttle high-frequency backend progress events (same pattern as
  // WhisperModelManager): at most one state update per 300ms, on a >=5%
  // jump, or at completion. Without this, every event mints new Map/Set
  // identities and re-renders every consumer (e.g. the whole
  // Settings > Summary tree) dozens of times per second during downloads.
  const progressThrottleRef = useRef<Map<string, { progress: number; timestamp: number }>>(new Map());

  /**
   * Set up event listeners for download progress
   * These persist for the lifetime of the app, unlike modal-scoped listeners
   */
  useEffect(() => {
    console.log('[OllamaDownloadContext] Setting up event listeners');
    const unsubscribers: (() => void)[] = [];

    const setupListeners = async () => {
      try {
        // Download progress
        const unlistenProgress = await listen<{ modelName: string; progress: number }>(
          'ollama-model-download-progress',
          (event) => {
            const { modelName, progress } = event.payload;
            const now = Date.now();
            const last = progressThrottleRef.current.get(modelName);
            const shouldUpdate =
              !last ||
              progress >= 100 ||
              now - last.timestamp > 300 ||
              Math.abs(progress - last.progress) >= 5;
            if (!shouldUpdate) return;

            progressThrottleRef.current.set(modelName, { progress, timestamp: now });
            console.log(`🔵 [OllamaDownloadContext] Progress for ${modelName}: ${progress}%`);

            setDownloadProgress(prev => {
              const newProgress = new Map(prev);
              newProgress.set(modelName, progress);
              return newProgress;
            });

            // Add to downloading set if not already there
            setDownloadingModels(prev => {
              if (prev.has(modelName)) return prev;
              const newSet = new Set(prev);
              newSet.add(modelName);
              return newSet;
            });
          }
        );
        unsubscribers.push(unlistenProgress);

        // Download complete
        const unlistenComplete = await listen<{ modelName: string }>(
          'ollama-model-download-complete',
          (event) => {
            const { modelName } = event.payload;
            console.log(`✅ [OllamaDownloadContext] Download complete for ${modelName}`);

            toast.success(`Model ${modelName} downloaded!`, {
              description: 'Model is now ready to use',
              duration: 4000
            });

            // Clear progress and remove from downloading set
            progressThrottleRef.current.delete(modelName);
            setDownloadProgress(prev => {
              const newProgress = new Map(prev);
              newProgress.delete(modelName);
              return newProgress;
            });

            setDownloadingModels(prev => {
              const newSet = new Set(prev);
              newSet.delete(modelName);
              return newSet;
            });
          }
        );
        unsubscribers.push(unlistenComplete);

        // Download error
        const unlistenError = await listen<{ modelName: string; error: string }>(
          'ollama-model-download-error',
          (event) => {
            const { modelName, error } = event.payload;
            console.error(`❌ [OllamaDownloadContext] Download error for ${modelName}:`, error);

            toast.error(`Download failed: ${modelName}`, {
              description: error,
              duration: 6000
            });

            // Clear progress and remove from downloading set
            progressThrottleRef.current.delete(modelName);
            setDownloadProgress(prev => {
              const newProgress = new Map(prev);
              newProgress.delete(modelName);
              return newProgress;
            });

            setDownloadingModels(prev => {
              const newSet = new Set(prev);
              newSet.delete(modelName);
              return newSet;
            });
          }
        );
        unsubscribers.push(unlistenError);

        console.log('[OllamaDownloadContext] Event listeners set up successfully');
      } catch (error) {
        console.error('[OllamaDownloadContext] Failed to set up event listeners:', error);
      }
    };

    setupListeners();

    return () => {
      console.log('[OllamaDownloadContext] Cleaning up event listeners');
      unsubscribers.forEach(unsub => unsub());
    };
  }, []);

  const contextValue: OllamaDownloadContextType = {
    downloadProgress,
    downloadingModels,
    isDownloading: (modelName: string) => downloadingModels.has(modelName),
    getProgress: (modelName: string) => downloadProgress.get(modelName),
  };

  return (
    <OllamaDownloadContext.Provider value={contextValue}>
      {children}
    </OllamaDownloadContext.Provider>
  );
}
