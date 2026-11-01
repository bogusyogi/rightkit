import { useState, useSyncExternalStore, type ReactNode } from 'react';
import { AppShell, Badge, Pill, StatusRow } from '@rightkit/react-shell/react';
import type { NavGroup } from '@rightkit/react-shell';
import { jobIsActive, useApp } from '../lib/app-state';
import { doctorHealthy } from '../lib/doctor';
import { VIEW_TITLES, settingsSectionOrDefault, type SettingsSectionId, type ViewId } from '../lib/nav';
import { themeStore } from '../lib/theme';
import { Generate } from '../views/Generate';
import { Gallery } from '../views/Gallery';
import { Jobs } from '../views/Jobs';
import { Settings } from '../views/Settings';
import { ComingSoon } from '../views/ComingSoon';
import { Kdp } from '../views/Kdp';
import { Motion } from '../views/Motion';
import { Production } from '../views/Production';
import { Voices } from '../views/Voices';
import { Avatars } from '../views/Avatars';
import { Recipes } from '../views/Recipes';
import { TemplateSwap } from '../views/TemplateSwap';
import { WINDOW_CONTROLS } from '../lib/windowControls';
import { IconAvatar, IconBook, IconGrid, IconImage, IconLayers, IconList, IconMic, IconMotion, IconRecipe, IconVideo } from './icons';
import { usd } from './ui';

interface NavItem {
  id: ViewId;
  label: string;
  icon: ReactNode;
  soon?: boolean;
}

const GROUPS: Array<{ title: string; items: NavItem[] }> = [
  { title: 'Workflows', items: [{ id: 'kdp', label: 'KDP', icon: <IconBook /> }, { id: 'production', label: 'Production', icon: <IconLayers /> }] },
  { title: 'Create', items: [{ id: 'image', label: 'Image', icon: <IconImage /> }, { id: 'video', label: 'Video', icon: <IconVideo /> }, { id: 'voice', label: 'Voice', icon: <IconMic /> }, { id: 'voices', label: 'Voice library', icon: <IconMic /> }, { id: 'music', label: 'Music', icon: <IconMic /> }, { id: 'sfx', label: 'Sound FX', icon: <IconMic /> }, { id: 'upscale', label: 'Upscale', icon: <IconImage /> }, { id: 'lipsync', label: 'Lipsync', icon: <IconVideo /> }, { id: 'avatars', label: 'Avatars', icon: <IconAvatar /> }, { id: 'motion', label: 'Motion', icon: <IconMotion /> }, { id: 'recipes', label: 'Recipes', icon: <IconRecipe /> }, { id: 'swap', label: 'Template swap', icon: <IconRecipe /> }] },
  { title: 'Library', items: [{ id: 'gallery', label: 'Gallery', icon: <IconGrid /> }, { id: 'jobs', label: 'Jobs', icon: <IconList /> }] }
];


export function DesktopWorkspace({ onOpenSetup }: { onOpenSetup: () => void }) {
  const app = useApp();
  const [view, setView] = useState<ViewId>(() => {
    const qa = import.meta.env.DEV ? (new URLSearchParams(window.location.search).get('qaView') as ViewId | null) : null;
    return qa && qa in VIEW_TITLES ? qa : 'image';
  });
  // The Settings section a navigation asked for, and a counter that remounts Settings on every visit so it opens there.
  const [settingsSection, setSettingsSection] = useState<SettingsSectionId | undefined>(undefined);
  const [settingsVisit, setSettingsVisit] = useState(0);
  const navigate = (next: ViewId, section?: SettingsSectionId) => {
    setView(next);
    setSettingsSection(settingsSectionOrDefault(section));
    setSettingsVisit((n) => n + 1);
  };
  const theme = useSyncExternalStore(themeStore.subscribe, themeStore.read, themeStore.read);

  const active = app.jobs.filter(jobIsActive).length;
  const health = doctorHealthy(app.doctor, app.doctorError);
  const noKeys = app.configured.size === 0;
  const spendLabel = app.spend ? `${usd(app.spend.actual_usd + app.spend.reserved_usd)} / $${app.spend.cap_usd.toFixed(0)} today` : null;

  let body: ReactNode;
  switch (view) {
    case 'image':
    case 'video':
    case 'voice':
    case 'music':
    case 'sfx':
    case 'upscale':
    case 'lipsync':
      body = <Generate kind={view} onNavigate={navigate} />;
      break;
    case 'kdp':
      body = <Kdp />;
      break;
    case 'production':
      body = <Production onNavigate={navigate} />;
      break;
    case 'motion':
      body = <Motion />;
      break;
    case 'voices':
      body = <Voices />;
      break;
    case 'avatars':
      body = <Avatars />;
      break;
    case 'recipes':
      body = <Recipes />;
      break;
    case 'swap':
      body = <TemplateSwap onNavigate={navigate} />;
      break;
    case 'gallery':
      body = <Gallery />;
      break;
    case 'jobs':
      body = <Jobs />;
      break;
    case 'settings':
      body = <Settings key={settingsVisit} onOpenSetup={onOpenSetup} initialSection={settingsSection} />;
      break;
    default:
      body = <ComingSoon id={view} />;
  }

  const navGroups: NavGroup[] = GROUPS.map((g) => ({
    title: g.title,
    items: g.items.map((i) => ({
      id: i.id,
      label: i.label,
      icon: i.icon,
      badge: i.soon ? <Badge>soon</Badge>
        : i.id === 'jobs' && active > 0 ? <Badge tone="accent">{active}</Badge>
        : i.id === 'image' && app.localProfiles.some((p) => p.install_state === 'installed') ? <Badge tone="ok">local</Badge>
        : undefined
    }))
  }));

  return (
    <AppShell
      groups={navGroups}
      activeId={view}
      onNavigate={(id) => navigate(id as ViewId)}
      title={VIEW_TITLES[view]}
      searchMode="filter"
      titlebarEnd={<>
        {noKeys ? <Pill tone="warn" onClick={() => navigate('settings')}>No API keys yet</Pill> : null}
        {spendLabel ? <Pill>{spendLabel}</Pill> : null}
        <Pill>Queue · {active ? `${active} running` : 'idle'}</Pill>
      </>}
      statusFooter={<>
        <StatusRow tone={health === 'ok' ? 'ok' : health === 'bad' ? 'bad' : 'idle'} label="Engine" value={health === 'ok' ? 'healthy' : health === 'bad' ? 'problem' : 'checking'} />
        <StatusRow tone={noKeys ? 'warn' : 'ok'} label="Providers" value={`${app.configured.size} key${app.configured.size === 1 ? '' : 's'}`} />
        <StatusRow tone={active ? 'warn' : 'ok'} label="Queue" value={active ? `${active} running` : 'idle'} />
      </>}
      wordmark={<>Gen<span>Right</span></>}
      theme={theme}
      onThemeChange={themeStore.save}
      onOpenSettings={() => navigate('settings')}
      settingsActive={view === 'settings'}
      bridge={{ window: WINDOW_CONTROLS }}
      sidebarToggle={false}
    >
      {body}
    </AppShell>
  );
}
