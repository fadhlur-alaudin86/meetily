"use client"

import { useEffect, useState } from "react"
import { invoke } from "@tauri-apps/api/core"
import { listen } from "@tauri-apps/api/event"

type QuitChoice = 'background' | 'kill';

/**
 * Global quit confirmation dialog. The backend emits `show-quit-dialog`
 * when the main window close is requested and the stored quit behavior
 * is "ask". Mounted once in the root layout so it works from any page.
 */
export function QuitDialog() {
  const [isOpen, setIsOpen] = useState(false);
  const [dontAskAgain, setDontAskAgain] = useState(false);
  const [isActing, setIsActing] = useState(false);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    const setup = async () => {
      try {
        unlisten = await listen('show-quit-dialog', () => {
          setDontAskAgain(false);
          setIsOpen(true);
        });
      } catch (err) {
        console.error('Failed to listen for show-quit-dialog:', err);
      }
    };
    setup();
    return () => {
      unlisten?.();
    };
  }, []);

  const handleChoice = async (choice: QuitChoice) => {
    if (isActing) return;
    setIsActing(true);
    try {
      if (dontAskAgain) {
        await invoke('api_set_quit_behavior', { behavior: choice });
      }
      if (choice === 'background') {
        await invoke('api_quit_background');
      } else {
        await invoke('api_quit_app');
      }
      // api_quit_background hides the window; close the dialog too.
      // api_quit_app terminates the process, so nothing after matters.
      setIsOpen(false);
    } catch (err) {
      console.error('Failed to quit:', err);
    } finally {
      setIsActing(false);
    }
  };

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 bg-black bg-opacity-50 flex items-center justify-center z-50">
      <div className="bg-white rounded-lg p-6 max-w-md w-full mx-4 shadow-xl">
        <h2 className="text-xl font-semibold mb-3">Quit Meetily?</h2>
        <p className="text-gray-600 mb-4">
          Quit to background to keep recording from the tray, or quit completely to stop the app.
        </p>

        <div className="flex flex-col gap-2 mb-5">
          <button
            onClick={() => handleChoice('background')}
            disabled={isActing}
            autoFocus
            className="w-full px-4 py-2 text-sm font-medium text-white bg-blue-600 hover:bg-blue-700 rounded-md transition-colors disabled:opacity-50"
          >
            Quit to background
          </button>
          <button
            onClick={() => handleChoice('kill')}
            disabled={isActing}
            className="w-full px-4 py-2 text-sm font-medium text-gray-700 bg-gray-200 hover:bg-gray-300 rounded-md transition-colors disabled:opacity-50"
          >
            Quit completely
          </button>
        </div>

        <div className="flex items-center space-x-2.5">
          <input
            type="checkbox"
            id="quit-dont-ask-checkbox"
            checked={dontAskAgain}
            onChange={(e) => setDontAskAgain(e.target.checked)}
            className="w-4 h-4 text-blue-600 rounded border-gray-300 focus:ring-blue-500 cursor-pointer"
          />
          <label
            htmlFor="quit-dont-ask-checkbox"
            className="text-xs text-gray-700 cursor-pointer select-none"
          >
            Don&apos;t ask me again
          </label>
        </div>
      </div>
    </div>
  );
}
