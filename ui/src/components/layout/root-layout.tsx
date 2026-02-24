import { useState } from 'react'
import { Outlet } from 'react-router-dom'
import { Menu } from 'lucide-react'
import { Sidebar } from './sidebar'
import { Button } from '@/components/ui'

export function RootLayout() {
  const [sidebarOpen, setSidebarOpen] = useState(false)

  const closeSidebar = () => setSidebarOpen(false)

  return (
    <div className="flex h-screen">
      {/* Desktop sidebar (always visible at md+) */}
      <div className="hidden md:block">
        <Sidebar />
      </div>

      {/* Mobile sidebar (overlay) */}
      <div
        className={`fixed inset-0 z-40 bg-black/50 transition-opacity md:hidden ${
          sidebarOpen ? 'opacity-100' : 'pointer-events-none opacity-0'
        }`}
        onClick={closeSidebar}
      />
      <div
        className={`fixed inset-y-0 left-0 z-50 transition-transform duration-200 md:hidden ${
          sidebarOpen ? 'translate-x-0' : '-translate-x-full'
        }`}
        onClick={(e) => {
          // Close sidebar when a navigation link is clicked
          if ((e.target as HTMLElement).closest('a')) closeSidebar()
        }}
      >
        <Sidebar onClose={closeSidebar} />
      </div>

      <main className="flex-1 overflow-auto">
        {/* Mobile header with hamburger */}
        <div className="sticky top-0 z-30 flex h-14 items-center border-b border-border bg-background px-4 md:hidden">
          <Button
            variant="ghost"
            size="icon"
            onClick={() => setSidebarOpen(true)}
          >
            <Menu className="h-5 w-5" />
          </Button>
          <img src="/logo.png" alt="Barbacane" className="h-7 w-7 ml-3 mr-2" />
          <span className="text-lg font-bold text-gradient">Barbacane</span>
        </div>
        <Outlet />
      </main>
    </div>
  )
}
