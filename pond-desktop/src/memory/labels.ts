// The words a memory card shows for its category: the same ones the phone uses, so a memory reads
// the same on both screens. The tier is not a category; only the permanent one earns a marker.

import i18n from "../i18n";
import type { MemoryFragment, MemorySegment } from "../api/types";

/** The friendly name of a memory's segment, as the phone words it. */
export function segmentLabel(segment: MemorySegment): string {
  switch (segment) {
    case "identity": return i18n.t("memory.segment.identity");
    case "preference": return i18n.t("memory.segment.preference");
    case "correction": return i18n.t("memory.segment.correction");
    case "relationship": return i18n.t("memory.segment.relationship");
    case "project": return i18n.t("memory.segment.project");
    case "routine": return i18n.t("memory.segment.routine");
    case "knowledge": return i18n.t("memory.segment.knowledge");
    case "context": return i18n.t("memory.segment.context");
  }
}

/** Whether the pond keeps this memory forever: its `permanent` tier, shown as "Kept". */
export function isKept(memory: Pick<MemoryFragment, "tier">): boolean {
  return memory.tier === "permanent";
}
