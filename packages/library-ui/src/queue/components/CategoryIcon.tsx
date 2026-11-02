import { FileText, Globe, Users, Video, type LucideIcon } from "lucide-react";
import type { JobCategory } from "../lib/jobDisplay.js";

/** The one icon per category. `other` has no icon. */
export const CATEGORY_ICON: Record<JobCategory, LucideIcon | null> = {
  media: Video,
  social: Users,
  document: FileText,
  web: Globe,
  other: null,
};

export function categoryIcon(category: string): LucideIcon | null {
  return CATEGORY_ICON[category as JobCategory] ?? null;
}
