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

### 4. UX Improvements ✅
- [x] Reusable `EmptyState`, `SearchInput`, `Breadcrumb`, `DropZone` components
- [x] `useDebounce` hook and shared time formatting utilities
- [x] Search and filtering on specs, plugins, and projects pages
- [x] Breadcrumb navigation across all pages
- [x] Drag-and-drop spec upload (empty state + persistent)
- [x] Responsive sidebar with mobile close button
- [x] On-demand spec compliance re-checking (`GET /specs/{id}/compliance`)
- [x] Build logs viewer with structured display and level filtering
- [x] Data plane health indicators with auto-refresh

### 5. UX Improvements (Batch 2) ✅
- [x] Error boundaries with React Router `errorElement` at root and project levels
- [x] `ConfirmDialog` component and `useConfirm` hook (12 call sites)
- [x] `CodeBlock` with `shiki` syntax highlighting for YAML/JSON in spec viewers
- [x] Middleware chain preview on operations page (correct merge semantics)
- [x] Undo/redo in edit dialogs (`useHistory` hook + keyboard shortcuts)
- [x] Playwright E2E tests (smoke navigation + spec workflow)
- [x] CI jobs for UI unit tests and E2E tests

## Future

### Authentication (Production)
- [ ] Replace mock auth with OIDC (`oidc-client-ts` or `react-oidc-context`)
- [ ] Configure OIDC provider (Auth0, Keycloak, etc.)
- [ ] Handle token refresh
- [ ] Add role-based access control

### Features
- [ ] Compilation progress indicator
- [ ] Artifact deployment workflow
- [ ] Plugin configuration UI
