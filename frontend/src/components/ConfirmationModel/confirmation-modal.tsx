import React, { useState, useEffect } from 'react';

interface ConfirmationModalProps {
  onConfirm: (deleteFiles?: boolean) => void;
  onCancel: () => void;
  text: string;
  isOpen: boolean;
  showDeleteFilesOption?: boolean;
}

export function ConfirmationModal({
  onConfirm,
  onCancel,
  text,
  isOpen,
  showDeleteFilesOption = false,
}: ConfirmationModalProps) {
  const [deleteFiles, setDeleteFiles] = useState(false);

  useEffect(() => {
    if (isOpen) {
      setDeleteFiles(false);
    }
  }, [isOpen]);

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 bg-black bg-opacity-50 flex items-center justify-center z-50">
      <div className="bg-white rounded-lg p-6 max-w-md w-full mx-4 shadow-xl">
        <h2 className="text-xl font-semibold mb-3">Confirm Delete</h2>
        <p className="text-gray-600 mb-4">{text}</p>

        {showDeleteFilesOption && (
          <div className="flex items-center space-x-2.5 p-3 mb-5 bg-red-50/60 border border-red-100 rounded-md">
            <input
              type="checkbox"
              id="delete-files-checkbox"
              checked={deleteFiles}
              onChange={(e) => setDeleteFiles(e.target.checked)}
              className="w-4 h-4 text-red-600 rounded border-gray-300 focus:ring-red-500 cursor-pointer"
            />
            <label
              htmlFor="delete-files-checkbox"
              className="text-xs text-gray-700 cursor-pointer select-none"
            >
              Hapus juga file rekaman & folder dari disk
            </label>
          </div>
        )}

        <div className="flex justify-end space-x-3">
          <button
            onClick={onCancel}
            className="px-4 py-2 text-sm text-gray-600 hover:bg-gray-100 rounded-md transition-colors"
          >
            Cancel
          </button>
          <button
            onClick={() => onConfirm(deleteFiles)}
            className="px-4 py-2 text-sm bg-red-600 text-white hover:bg-red-700 rounded-md transition-colors font-medium shadow-sm"
          >
            Delete
          </button>
        </div>
      </div>
    </div>
  );
}
