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

## In Progress

### 3. Authentication (OpenID Connect)
- [ ] Choose OIDC library (e.g., `oidc-client-ts`, `react-oidc-context`)
- [ ] Add auth provider wrapper
- [ ] Protect routes with auth guard
- [ ] Add login/logout flow
- [ ] Store tokens securely
- [ ] Add user info to sidebar
- [ ] Handle token refresh

## Future

- [ ] Spec upload with drag & drop
- [ ] Spec editor with syntax highlighting
- [ ] Compilation progress indicator
- [ ] Artifact deployment workflow
- [ ] Plugin configuration UI
