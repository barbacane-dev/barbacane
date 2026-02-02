import {
  createContext,
  useContext,
  useState,
  useEffect,
  type ReactNode,
} from 'react'
import type { User, AuthState } from './types'

const STORAGE_KEY = 'barbacane-auth'

// Mock user for development
const MOCK_USER: User = {
  id: '1',
  email: 'admin@barbacane.dev',
  name: 'Admin User',
}

interface AuthContextValue extends AuthState {
  login: (email: string, password: string) => Promise<void>
  logout: () => void
}

const AuthContext = createContext<AuthContextValue | null>(null)

export function AuthProvider({ children }: { children: ReactNode }) {
  const [state, setState] = useState<AuthState>({
    user: null,
    isAuthenticated: false,
    isLoading: true,
  })

  // Check for existing session on mount
  useEffect(() => {
    const stored = localStorage.getItem(STORAGE_KEY)
    if (stored) {
      try {
        const user = JSON.parse(stored) as User
        setState({ user, isAuthenticated: true, isLoading: false })
      } catch {
        localStorage.removeItem(STORAGE_KEY)
        setState({ user: null, isAuthenticated: false, isLoading: false })
      }
    } else {
      setState({ user: null, isAuthenticated: false, isLoading: false })
    }
  }, [])

  const login = async (email: string, _password: string) => {
    // Mock login - accepts any credentials in dev
    // Replace with real OIDC flow later
    await new Promise((resolve) => setTimeout(resolve, 500)) // Simulate network

    const user: User = {
      ...MOCK_USER,
      email,
      name: email.split('@')[0],
    }

    localStorage.setItem(STORAGE_KEY, JSON.stringify(user))
    setState({ user, isAuthenticated: true, isLoading: false })
  }

  const logout = () => {
    localStorage.removeItem(STORAGE_KEY)
    setState({ user: null, isAuthenticated: false, isLoading: false })
  }

  return (
    <AuthContext.Provider value={{ ...state, login, logout }}>
      {children}
    </AuthContext.Provider>
  )
}

export function useAuth() {
  const context = useContext(AuthContext)
  if (!context) {
    throw new Error('useAuth must be used within an AuthProvider')
  }
  return context
}
