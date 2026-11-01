import { ArrowClockwise } from "@phosphor-icons/react/dist/icons/ArrowClockwise";
import { ArrowsSplit } from "@phosphor-icons/react/dist/icons/ArrowsSplit";
import { CaretDown } from "@phosphor-icons/react/dist/icons/CaretDown";
import { Check } from "@phosphor-icons/react/dist/icons/Check";
import { Clock } from "@phosphor-icons/react/dist/icons/Clock";
import { Cloud } from "@phosphor-icons/react/dist/icons/Cloud";
import { Command } from "@phosphor-icons/react/dist/icons/Command";
import { Copy } from "@phosphor-icons/react/dist/icons/Copy";
import { Cube } from "@phosphor-icons/react/dist/icons/Cube";
import { DotsThree } from "@phosphor-icons/react/dist/icons/DotsThree";
import { DownloadSimple } from "@phosphor-icons/react/dist/icons/DownloadSimple";
import { File } from "@phosphor-icons/react/dist/icons/File";
import { FileText } from "@phosphor-icons/react/dist/icons/FileText";
import { Flag } from "@phosphor-icons/react/dist/icons/Flag";
import { Folder } from "@phosphor-icons/react/dist/icons/Folder";
import { GitBranch } from "@phosphor-icons/react/dist/icons/GitBranch";
import { GithubLogo } from "@phosphor-icons/react/dist/icons/GithubLogo";
import { Globe } from "@phosphor-icons/react/dist/icons/Globe";
import { GridFour } from "@phosphor-icons/react/dist/icons/GridFour";
import { HardDrives } from "@phosphor-icons/react/dist/icons/HardDrives";
import { Image } from "@phosphor-icons/react/dist/icons/Image";
import { Key } from "@phosphor-icons/react/dist/icons/Key";
import { LinkSimple } from "@phosphor-icons/react/dist/icons/LinkSimple";
import { List } from "@phosphor-icons/react/dist/icons/List";
import { ListChecks } from "@phosphor-icons/react/dist/icons/ListChecks";
import { Lock } from "@phosphor-icons/react/dist/icons/Lock";
import { MagnifyingGlass } from "@phosphor-icons/react/dist/icons/MagnifyingGlass";
import { Minus } from "@phosphor-icons/react/dist/icons/Minus";
import { Package } from "@phosphor-icons/react/dist/icons/Package";
import { Paperclip } from "@phosphor-icons/react/dist/icons/Paperclip";
import { PaperPlaneTilt } from "@phosphor-icons/react/dist/icons/PaperPlaneTilt";
import { Pause } from "@phosphor-icons/react/dist/icons/Pause";
import { PencilSimple } from "@phosphor-icons/react/dist/icons/PencilSimple";
import { Play } from "@phosphor-icons/react/dist/icons/Play";
import { Plus } from "@phosphor-icons/react/dist/icons/Plus";
import { Pulse } from "@phosphor-icons/react/dist/icons/Pulse";
import { Sidebar } from "@phosphor-icons/react/dist/icons/Sidebar";
import { SidebarSimple } from "@phosphor-icons/react/dist/icons/SidebarSimple";
import { SlidersHorizontal } from "@phosphor-icons/react/dist/icons/SlidersHorizontal";
import { Sparkle } from "@phosphor-icons/react/dist/icons/Sparkle";
import { Square } from "@phosphor-icons/react/dist/icons/Square";
import { Stop } from "@phosphor-icons/react/dist/icons/Stop";
import { Sun } from "@phosphor-icons/react/dist/icons/Sun";
import { Terminal } from "@phosphor-icons/react/dist/icons/Terminal";
import { Target } from "@phosphor-icons/react/dist/icons/Target";
import { Trash } from "@phosphor-icons/react/dist/icons/Trash";
import { UploadSimple } from "@phosphor-icons/react/dist/icons/UploadSimple";
import { User } from "@phosphor-icons/react/dist/icons/User";
import { Warning } from "@phosphor-icons/react/dist/icons/Warning";
import { X } from "@phosphor-icons/react/dist/icons/X";
import { Moon } from "@phosphor-icons/react/dist/icons/Moon";
import type { Icon as PhosphorIcon } from "@phosphor-icons/react";

export type IconName =
  | "folder"
  | "settings"
  | "tasks"
  | "activity"
  | "plus"
  | "refresh"
  | "close"
  | "minimize"
  | "maximize"
  | "search"
  | "terminal"
  | "cloud"
  | "git"
  | "key"
  | "lock"
  | "send"
  | "spark"
  | "user"
  | "chevron"
  | "branch"
  | "server"
  | "panel"
  | "panel-right"
  | "edit"
  | "trash"
  | "more"
  | "pause"
  | "play"
  | "flag"
  | "download"
  | "upload"
  | "copy"
  | "check"
  | "attachment"
  | "file"
  | "command"
  | "model"
  | "queue"
  | "stop"
  | "clock"
  | "diff"
  | "alert"
  | "image"
  | "link"
  | "document"
  | "package"
  | "github"
  | "skill"
  | "sun"
  | "moon"
  | "browser"
  | "target";

const icons: Record<IconName, PhosphorIcon> = {
  folder: Folder, settings: SlidersHorizontal, tasks: ListChecks, activity: Pulse, plus: Plus,
  refresh: ArrowClockwise, close: X, minimize: Minus, maximize: Square, search: MagnifyingGlass,
  terminal: Terminal, cloud: Cloud, git: GitBranch, key: Key, lock: Lock, send: PaperPlaneTilt,
  spark: Sparkle, user: User, chevron: CaretDown, branch: GitBranch, server: HardDrives, panel: Sidebar,
  "panel-right": SidebarSimple, edit: PencilSimple, trash: Trash, more: DotsThree, pause: Pause,
  play: Play, flag: Flag, download: DownloadSimple, upload: UploadSimple, copy: Copy, check: Check,
  attachment: Paperclip, file: File, command: Command, model: GridFour, queue: List, stop: Stop,
  clock: Clock, diff: ArrowsSplit, alert: Warning, image: Image, link: LinkSimple, document: FileText,
  package: Package, github: GithubLogo, skill: Cube, sun: Sun, moon: Moon, browser: Globe, target: Target,
};

export default function Icon({ name, size = 16 }: { name: IconName; size?: number }) {
  const Glyph = icons[name];
  return <Glyph size={size} weight="regular" color="currentColor" aria-hidden="true" />;
}
