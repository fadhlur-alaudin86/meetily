'use client';

import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'sonner';
import { ModelConfig, ModelSettingsModal } from '@/components/ModelSettingsModal';
import { SummaryLanguageSettings } from '@/components/SummaryLanguageSettings';
import { Switch } from './ui/switch';
import { useConfig, GpuHardwareInfo } from '@/contexts/ConfigContext';

interface SummaryModelSettingsProps {
  refetchTrigger?: number; // Change this to trigger refetch
}

export function SummaryModelSettings({ refetchTrigger }: SummaryModelSettingsProps) {
  const [modelConfig, setModelConfig] = useState<ModelConfig>({
    provider: 'ollama',
    model: 'llama3.2:latest',
    whisperModel: 'large-v3',
    apiKey: null,
    ollamaEndpoint: null
  });

  const { isAutoSummary, toggleIsAutoSummary, summaryChunkSize, setSummaryChunkSize } = useConfig();
  const [gpuInfo, setGpuInfo] = useState<GpuHardwareInfo | null>(null);
  const [isDetectingGpu, setIsDetectingGpu] = useState<boolean>(false);

  const detectGpu = useCallback(async (applyRecommended: boolean = false) => {
    setIsDetectingGpu(true);
    try {
      const info = await invoke<GpuHardwareInfo>('detect_gpu_hardware');
      setGpuInfo(info);
      if (applyRecommended && info?.recommended_chunk_size) {
        setSummaryChunkSize(info.recommended_chunk_size);
        toast.success(`GPU detected: ${info.detected_gpu}. Chunk size set to ${info.recommended_chunk_size.toLocaleString()} tokens.`);
      }
    } catch (err) {
      console.error('Failed to detect GPU hardware:', err);
      if (applyRecommended) {
        toast.error('Failed to detect GPU hardware');
      }
    } finally {
      setIsDetectingGpu(false);
    }
  }, [setSummaryChunkSize]);

  useEffect(() => {
    detectGpu(false);
  }, [detectGpu]);

  // Reusable fetch function
  const fetchModelConfig = useCallback(async () => {
    try {
      const data = await invoke('api_get_model_config') as any;
      if (data && data.provider !== null) {
        // Fetch API key if not included and provider requires it
        if (data.provider !== 'ollama' && data.provider !== 'builtin-ai' && !data.apiKey) {
          try {
            const apiKeyData = await invoke('api_get_api_key', {
              provider: data.provider
            }) as string;
            data.apiKey = apiKeyData;
          } catch (err) {
            console.error('Failed to fetch API key:', err);
          }
        }
        // Fetch Custom OpenAI config if that's the active provider
        if (data.provider === 'custom-openai') {
          try {
            const customConfig = (await invoke('api_get_custom_openai_config')) as any;
            if (customConfig) {
              data.customOpenAIDisplayName = customConfig.displayName || null;
              data.customOpenAIEndpoint = customConfig.endpoint || null;
              data.customOpenAIModel = customConfig.model || null;
              data.customOpenAIApiKey = customConfig.apiKey || null;
              data.maxTokens = customConfig.maxTokens || null;
              data.temperature = customConfig.temperature || null;
              data.topP = customConfig.topP || null;
              // For custom-openai, model field should match customOpenAIModel
              data.model = customConfig.model || data.model;
            }
          } catch (err) {
            console.error('Failed to fetch custom OpenAI config:', err);
          }
        }
        setModelConfig(data);
      }
    } catch (error) {
      console.error('Failed to fetch model config:', error);
      toast.error('Failed to load model settings');
    }
  }, []);

  // Fetch on mount
  useEffect(() => {
    fetchModelConfig();
  }, [fetchModelConfig]);

  // Refetch when trigger changes (optional external control)
  useEffect(() => {
    if (refetchTrigger !== undefined && refetchTrigger > 0) {
      fetchModelConfig();
    }
  }, [refetchTrigger, fetchModelConfig]);

  // Listen for model config updates from other components
  useEffect(() => {
    const setupListener = async () => {
      const { listen } = await import('@tauri-apps/api/event');
      const unlisten = await listen<ModelConfig>('model-config-updated', (event) => {
        console.log('SummaryModelSettings received model-config-updated event:', event.payload);
        setModelConfig(event.payload);
      });

      return unlisten;
    };

    let cleanup: (() => void) | undefined;
    setupListener().then(fn => cleanup = fn);

    return () => {
      cleanup?.();
    };
  }, []);

  // Save handler
  const handleSaveModelConfig = async (config: ModelConfig) => {
    try {
      await invoke('api_save_model_config', {
        provider: config.provider,
        model: config.model,
        whisperModel: config.whisperModel,
        apiKey: config.apiKey,
        ollamaEndpoint: config.ollamaEndpoint,
      });

      setModelConfig(config);

      // Emit event to sync other components
      const { emit } = await import('@tauri-apps/api/event');
      await emit('model-config-updated', config);

      toast.success('Model settings saved successfully');
    } catch (error) {
      console.error('Error saving model config:', error);
      toast.error('Failed to save model settings');
    }
  };

  return (
    <div className='flex flex-col gap-4'>
      <div className="bg-white rounded-lg border border-gray-200 p-6 shadow-sm">
        <div className="flex items-center justify-between">
          <div>
            <h3 className="text-lg font-semibold text-gray-900 mb-2">Auto Summary</h3>
            <p className="text-sm text-gray-600">Auto Generating summary after meeting completion(Stopping)</p>
          </div>
          <Switch checked={isAutoSummary} onCheckedChange={toggleIsAutoSummary} />
        </div>
      </div>

      <SummaryLanguageSettings />
 
       <div className="bg-white rounded-lg border border-gray-200 p-6 shadow-sm">
        <div className="flex flex-col gap-3">
          <div className="flex items-start justify-between">
            <div>
              <h3 className="text-lg font-semibold text-gray-900 mb-1">Transcript Chunk Size</h3>
              <p className="text-sm text-gray-600">
                Maximum tokens per chunk when generating summaries from long transcripts.
              </p>
            </div>
            <button
              type="button"
              onClick={() => detectGpu(true)}
              disabled={isDetectingGpu}
              className="inline-flex items-center px-3 py-1.5 text-xs font-medium text-blue-700 bg-blue-50 border border-blue-200 rounded-md hover:bg-blue-100 disabled:opacity-50 transition-colors cursor-pointer"
            >
              {isDetectingGpu ? 'Detecting GPU...' : 'Auto-detect GPU'}
            </button>
          </div>

          {gpuInfo && (
            <div className="flex flex-wrap items-center gap-2 p-2.5 bg-gray-50 border border-gray-200 rounded-md text-xs text-gray-700">
              <span className="font-semibold text-gray-900">Detected Hardware:</span>
              <span className="px-2 py-0.5 bg-white border border-gray-200 rounded font-mono text-gray-800">
                {gpuInfo.detected_gpu}
              </span>
              {gpuInfo.total_vram_mb !== null && (
                <span className="px-2 py-0.5 bg-white border border-gray-200 rounded text-gray-600">
                  {Math.round(gpuInfo.total_vram_mb / 1024)} GB VRAM
                </span>
              )}
              <span className="text-gray-400">|</span>
              <span className="text-gray-600">
                Recommended: <strong className="text-blue-700">{gpuInfo.recommended_chunk_size.toLocaleString()} tokens</strong>
              </span>
            </div>
          )}

          <div className="flex items-center gap-4 mt-2">
            <input
              type="range"
              min={1000}
              max={32768}
              step={500}
              value={summaryChunkSize}
              onChange={(e) => setSummaryChunkSize(Number(e.target.value))}
              className="flex-1 h-2 bg-gray-200 rounded-lg appearance-none cursor-pointer accent-blue-600"
            />
            <div className="flex items-center gap-1.5">
              <input
                type="number"
                min={1000}
                max={32768}
                step={500}
                value={summaryChunkSize}
                onChange={(e) => {
                  const val = Number(e.target.value);
                  if (!isNaN(val) && val >= 500 && val <= 32768) {
                    setSummaryChunkSize(val);
                  }
                }}
                className="w-24 px-3 py-1.5 text-sm font-mono border border-gray-300 rounded-md focus:outline-none focus:ring-2 focus:ring-blue-500 text-right"
              />
              <span className="text-xs text-gray-500 font-medium">tokens</span>
            </div>
          </div>

          <div className="flex justify-between text-xs text-gray-400 px-0.5">
            <span>1,000 (Low VRAM / 4GB)</span>
            <span>Recommended: {gpuInfo?.recommended_chunk_size.toLocaleString() ?? '3,000'}</span>
            <span>32,768 (Full Context / 24GB+)</span>
          </div>

          <p className="text-xs text-gray-500 mt-1">
            Transcripts larger than this threshold are automatically split into sentence-bounded chunks with context overlap, summarized individually, and combined into final meeting notes. Lower values (e.g. 3,000) prevent out-of-memory crashes on GPUs with 4GB VRAM, while higher values leverage larger GPUs.
          </p>
        </div>
      </div>

      <div className="bg-white rounded-lg border border-gray-200 p-6 shadow-sm">
        <h3 className="text-lg font-semibold mb-4">Summary Model Configuration</h3>
        <p className="text-sm text-gray-600 mb-6">
          Configure the AI model used for generating meeting summaries.
        </p>

        <ModelSettingsModal
          modelConfig={modelConfig}
          setModelConfig={setModelConfig}
          onSave={handleSaveModelConfig}
          skipInitialFetch={true}
        />
      </div>
    </div>
  );
}
