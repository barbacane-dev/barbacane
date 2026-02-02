# Control Plane UI - TODO

## Completed

### 1. Custom Color Theme ✅
- [x] Define brand colors (primary: cyan, secondary: purple, accent: magenta)
- [x] Update CSS variables in `index.css`
- [x] Apply to sidebar with gradient title and glow effects

### 2. Light/Dark Theme Toggle ✅
- [x] Add `useTheme` hook with localStorage persistence
- [x] Add toggle button in sidebar
- [x] CSS variables for both dark (default) and light modes

### 3. Authentication (Mock) ✅
- [x] Add AuthProvider with mock login
- [x] Add ProtectedRoute component
- [x] Add login page with form
- [x] Store auth state in localStorage
- [x] Add user info to sidebar
- [x] Add logout functionality

## Future

### Authentication (Production)
- [ ] Replace mock auth with OIDC (`oidc-client-ts` or `react-oidc-context`)
- [ ] Configure OIDC provider (Auth0, Keycloak, etc.)
- [ ] Handle token refresh
- [ ] Add role-based access control

### Features
- [ ] Spec upload with drag & drop
- [ ] Spec editor with syntax highlighting
- [ ] Compilation progress indicator
- [ ] Artifact deployment workflow
- [ ] Plugin configuration UI
