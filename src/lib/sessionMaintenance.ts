export interface SessionMaintenanceProgress {
  phase: "scanning" | "planning" | "validating" | "syncing" | "indexing" | "repairing" | "complete" | "partial";
  completed: number;
  total: number;
}

export interface SessionRepairIssue {
  relPath: string;
  message: string;
}

export interface SessionIdRepairPreview {
  token: string;
  scanned: number;
  files: { relPath: string; ids: number }[];
  issues: SessionRepairIssue[];
}

export interface SessionIdRepairResult {
  changedFiles: number;
  changedIds: number;
  remainingFiles: number;
  backupDir: string | null;
  issues: SessionRepairIssue[];
}
