// The workspace views' glyphs, from Lucide (lucide-react). One name per glyph the views use, so a view names a
// glyph without importing an app's icon set.
import { memo, type CSSProperties } from "react";
import {
  Calendar,
  Check,
  ChevronLeft,
  ChevronRight,
  Clock,
  Ellipsis,
  File,
  FileText,
  Folder,
  HardDrive,
  Image,
  LayoutGrid,
  List,
  Plus,
  Star,
  Tag,
  Trash2,
  Upload,
  UserPlus,
  Users,
  Video,
  type LucideIcon,
} from "lucide-react";

const GLYPHS = {
  calendar: Calendar,
  check: Check,
  "chevron-left": ChevronLeft,
  "chevron-right": ChevronRight,
  clock: Clock,
  doc: File,
  draft: FileText,
  drive: HardDrive,
  folder: Folder,
  grid: LayoutGrid,
  image: Image,
  list: List,
  more: Ellipsis,
  people: Users,
  "person-add": UserPlus,
  plus: Plus,
  star: Star,
  tag: Tag,
  trash: Trash2,
  upload: Upload,
  video: Video,
} satisfies Record<string, LucideIcon>;

export type WorkspaceIconName = keyof typeof GLYPHS;

/** A workspace glyph. Decorative (hidden from assistive technology); stroke follows the text colour. */
export const Icon = memo(function Icon({
  name,
  size = 16,
  className,
  style,
}: {
  name: WorkspaceIconName;
  size?: number;
  className?: string;
  style?: CSSProperties;
}) {
  const Glyph = GLYPHS[name];
  return <Glyph size={size} className={className} style={style} strokeWidth={1.75} aria-hidden="true" focusable="false" />;
});
