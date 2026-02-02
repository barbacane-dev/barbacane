import { NavLink } from 'react-router-dom'
import {
  FileCode,
  Package,
  Puzzle,
  Activity,
  Settings,
  Sun,
  Moon,
} from 'lucide-react'
import { cn } from '@/lib/utils'
import { useTheme } from '@/hooks'

const navigation = [
  { name: 'Specs', href: '/specs', icon: FileCode },
  { name: 'Plugins', href: '/plugins', icon: Puzzle },
  { name: 'Artifacts', href: '/artifacts', icon: Package },
  { name: 'Activity', href: '/activity', icon: Activity },
]

export function Sidebar() {
  const { theme, toggleTheme } = useTheme()

  return (
    <div className="flex h-full w-64 flex-col border-r border-sidebar-border bg-sidebar">
      <div className="flex h-16 items-center border-b border-sidebar-border px-6">
        <span className="text-xl font-bold text-gradient">Barbacane</span>
      </div>
      <nav className="flex-1 space-y-1 px-3 py-4">
        {navigation.map((item) => (
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
        ))}
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
    </div>
  )
}
