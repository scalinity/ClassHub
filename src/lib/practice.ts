import { invoke } from "@tauri-apps/api/core";

import { queryClient } from "@/lib/query";

/**
 * SPEC §8.3 — a practice exam's self-score. The exam's panel posts
 * `{exam, results: [{question, topic, correct}]}` to the frame's parent once
 * every section is totalled. The frame is sandboxed, so its origin reads
 * `null` and proves nothing: the message is matched against the frame the
 * viewer framed, and only for the exam it opened, and the payload goes to
 * the backend as model output to be checked there.
 *
 * One window listener, registered once; the viewer registers the frame it
 * shows through a ref callback and clears it when the frame unmounts, so
 * no effect is needed (SPEC §13).
 */

export interface ExamScore {
  correct: number;
  total: number;
  recordedAt: number;
  weakTopics: string[];
}

interface FramedExam {
  frame: HTMLIFrameElement;
  classId: number;
  scope: string;
  onScored: (score: ExamScore) => void;
  onRefused: (reason: string) => void;
}

let framed: FramedExam | null = null;

/** The frame showing a practice exam, or null once it is gone. */
export function setExamFrame(exam: FramedExam | null) {
  framed = exam;
}

export function recordPracticeResults(
  classId: number,
  scope: string,
  posted: unknown,
): Promise<ExamScore> {
  return invoke<ExamScore>("record_practice_results", { classId, scope, posted });
}

window.addEventListener("message", (event: MessageEvent) => {
  const exam = framed;
  if (exam === null || event.source !== exam.frame.contentWindow) return;
  const data: unknown = event.data;
  // The shape is checked backend-side; here only enough to tell the panel's
  // message from anything else a page might post.
  if (typeof data !== "object" || data === null || !("results" in data)) return;
  recordPracticeResults(exam.classId, exam.scope, data)
    .then((score) => {
      void queryClient.invalidateQueries({ queryKey: ["practice"] });
      exam.onScored(score);
    })
    .catch((e) => exam.onRefused(String(e)));
});
