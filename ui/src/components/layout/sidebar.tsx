import { NavLink } from 'react-router-dom'
import {
  LayoutDashboard,
  FolderKanban,
  Store,
  Activity,
  Settings,
  Sun,
  Moon,
  LogOut,
  User,
  FileText,
  ExternalLink,
  X,
} from 'lucide-react'
import { cn } from '@/lib/utils'
import { useTheme } from '@/hooks'
import { useAuth } from '@/lib/auth'

const mainNavigation = [
  { name: 'Dashboard', href: '/', icon: LayoutDashboard, end: true },
  { name: 'Projects', href: '/projects', icon: FolderKanban },
]

const globalNavigation = [
  { name: 'Plugin Registry', href: '/plugin-registry', icon: Store },
  { name: 'Activity', href: '/activity', icon: Activity },
  { name: 'API Docs', href: '/api/docs', icon: FileText, external: true },
]

interface SidebarProps {
  onClose?: () => void
}

export function Sidebar({ onClose }: SidebarProps) {
  const { theme, toggleTheme } = useTheme()
  const { user, logout } = useAuth()

  return (
    <div className="flex h-full w-64 flex-col border-r border-sidebar-border bg-sidebar">
      <div className="flex h-16 items-center border-b border-sidebar-border px-4">
        <img src="/logo.png" alt="Barbacane" className="h-10 w-10 mr-3" />
        <span className="text-xl font-bold text-gradient">Barbacane</span>
        {onClose && (
          <button
            onClick={onClose}
            className="ml-auto rounded-lg p-1.5 text-sidebar-foreground hover:bg-sidebar-accent md:hidden"
          >
            <X className="h-5 w-5" />
          </button>
        )}
      </div>
      <nav className="flex-1 px-3 py-4">
        {/* Main navigation */}
        <div className="space-y-1">
          {mainNavigation.map((item) => (
            <NavLink
              key={item.name}
              to={item.href}
              end={'end' in item ? item.end : undefined}
              className={({ isActive }) =>
                cn(
                  'flex items-center gap-3 rounded-lg px-3 py-2 text-sm font-medium transition-all',
                  isActive
                    ? 'bg-primary/10 text-primary glow-cyan'
                    : 'text-sidebar-foreground hover:bg-sidebar-accent hover:text-sidebar-accent-foreground'
                )
              }
            >
              <item.icon className="h-5 w-5" />
              {item.name}
            </NavLink>
          ))}
        </div>

        {/* Separator */}
        <div className="my-4 border-t border-sidebar-border" />

        {/* Global navigation */}
        <div className="space-y-1">
          <p className="px-3 py-1 text-xs font-medium text-muted-foreground uppercase tracking-wider">
            Global
          </p>
          {globalNavigation.map((item) =>
            item.external ? (
              <a
                key={item.name}
                href={item.href}
                target="_blank"
                rel="noopener noreferrer"
                className="flex items-center gap-3 rounded-lg px-3 py-2 text-sm font-medium transition-all text-sidebar-foreground hover:bg-sidebar-accent hover:text-sidebar-accent-foreground"
              >
                <item.icon className="h-5 w-5" />
                {item.name}
                <ExternalLink className="h-3 w-3 ml-auto opacity-50" />
              </a>
            ) : (
              <NavLink
                key={item.name}
                to={item.href}
                className={({ isActive }) =>
                  cn(
                    'flex items-center gap-3 rounded-lg px-3 py-2 text-sm font-medium transition-all',
                    isActive
                      ? 'bg-primary/10 text-primary glow-cyan'
                      : 'text-sidebar-foreground hover:bg-sidebar-accent hover:text-sidebar-accent-foreground'
                  )
                }
              >
                <item.icon className="h-5 w-5" />
                {item.name}
              </NavLink>
            )
          )}
        </div>
      </nav>
      <div className="border-t border-sidebar-border p-3 space-y-1">
        <NavLink
          to="/settings"
          className={({ isActive }) =>
            cn(
              'flex items-center gap-3 rounded-lg px-3 py-2 text-sm font-medium transition-all',
              isActive
                ? 'bg-primary/10 text-primary glow-cyan'
                : 'text-sidebar-foreground hover:bg-sidebar-accent hover:text-sidebar-accent-foreground'
            )
          }
        >
          <Settings className="h-5 w-5" />
          Settings
        </NavLink>
        <button
          onClick={toggleTheme}
          className="flex w-full items-center gap-3 rounded-lg px-3 py-2 text-sm font-medium text-sidebar-foreground transition-colors hover:bg-sidebar-accent hover:text-sidebar-accent-foreground"
        >
          {theme === 'dark' ? (
            <>
              <Sun className="h-5 w-5" />
              Light Mode
            </>
          ) : (
            <>
              <Moon className="h-5 w-5" />
              Dark Mode
            </>
          )}
        </button>
      </div>

      {/* User section */}
      <div className="border-t border-sidebar-border p-3">
        <div className="flex items-center gap-3 rounded-lg px-3 py-2">
          <div className="flex h-8 w-8 items-center justify-center rounded-full bg-primary/20 text-primary">
            <User className="h-4 w-4" />
          </div>
          <div className="flex-1 truncate">
            <div className="text-sm font-medium text-sidebar-foreground truncate">
              {user?.name}
            </div>
            <div className="text-xs text-muted-foreground truncate">
              {user?.email}
            </div>
          </div>
        </div>
        <button
          onClick={logout}
          className="mt-1 flex w-full items-center gap-3 rounded-lg px-3 py-2 text-sm font-medium text-sidebar-foreground transition-colors hover:bg-destructive/10 hover:text-destructive"
        >
          <LogOut className="h-5 w-5" />
          Sign out
        </button>
      </div>
    </div>
  )
}
