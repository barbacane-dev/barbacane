# Web UI

The Barbacane Control Plane includes a React-based web interface for managing your API gateway. This guide covers the features and workflows available in the UI.

## Getting Started

### Starting the UI

```bash
# Using Makefile (from project root)
make ui

# Or manually
cd ui && npm run dev
```

The UI runs at http://localhost:5173 and proxies API requests to the control plane at http://localhost:9090.

### Prerequisites

Before using the UI, ensure:

1. **Control Plane is running** - Start with `make control-plane`
2. **Database is ready** - Start PostgreSQL with `make db-up`
3. **Plugins are seeded** - Run `make seed-plugins` to populate the plugin registry

## Features Overview

| Feature | Description |
|---------|-------------|
| **Projects** | Create and manage API gateway projects |
| **Specs** | Upload and manage OpenAPI/AsyncAPI specifications |
| **Plugin Registry** | Browse available plugins with schemas |
| **Plugin Configuration** | Configure plugins per project with validation |
| **Builds** | Compile specs into deployable artifacts |
| **Deploy** | Deploy artifacts to connected data planes |
| **API Keys** | Manage authentication for data planes |

## Projects

Projects organize your API gateway configuration. Each project contains:

- **Specs** - API specifications to compile
- **Plugins** - Configured middleware and dispatchers
- **Builds** - Compilation history and artifacts
- **Data Planes** - Connected gateway instances

### Creating a Project

1. Navigate to **Projects** from the sidebar
2. Click **New Project**
3. Enter a name and optional description
4. Click **Create**

You can also create a project from a template by clicking **From Template**, which uses the `/init` endpoint to generate a starter API specification.

### Project Navigation

Each project has a tabbed interface:

| Tab | Description |
|-----|-------------|
| **Specs** | Manage API specifications for this project |
| **Plugins** | Configure which plugins to use and their settings |
| **Builds** | View compilation history and trigger new builds |
| **Deploy** | Deploy artifacts and manage connected data planes |
| **Settings** | Project settings and danger zone |

## Specs Management

### Uploading Specs

1. Navigate to a project's **Specs** tab
2. Click **Upload Spec**
3. Select an OpenAPI or AsyncAPI YAML/JSON file
4. The spec is parsed, validated, and stored

### Spec Features

- **Revision History** - Each upload creates a new revision
- **Content Preview** - View the raw spec content
- **Validation** - Specs are validated on upload
- **Type Detection** - Automatically detects OpenAPI vs AsyncAPI

### Global Specs View

The **Specs** page in the sidebar shows all specs across all projects. Use this for:

- Browsing all uploaded specifications
- Searching specs by name
- Viewing specs not yet assigned to projects

## Plugin Registry

The **Plugin Registry** shows all available WASM plugins:

### Plugin Types

| Type | Description |
|------|-------------|
| **Middleware** | Request/response processing (auth, rate limiting, CORS) |
| **Dispatcher** | Backend integration (HTTP upstream, Lambda, mock) |

### Plugin Information

Each plugin displays:

- **Name and Version** - Unique identifier
- **Description** - What the plugin does
- **Capabilities** - Required host functions (e.g., `http`, `log`, `kv`)
- **Config Schema** - JSON Schema for configuration (if defined)

### Deleting Plugins

Plugins can be deleted from the registry if they're not in use by any project. If a plugin is in use, you'll see an error message asking you to remove it from all projects first.

## Plugin Configuration

Configure plugins for each project from the **Plugins** tab.

### Adding a Plugin

1. Click **Add Plugin**
2. Select a plugin from the registry dropdown
3. The plugin's JSON Schema (if available) generates a configuration form
4. Fill in required and optional fields
5. Click **Add Plugin**

### Configuration Features

- **Schema-based forms** - Auto-generated from JSON Schema
- **Real-time validation** - Invalid configs are rejected before save
- **Enable/Disable** - Toggle plugins without removing configuration
- **Reorder** - Drag to change middleware execution order

### Example: Configuring Rate Limiting

1. Add the `rate-limit` plugin
2. Configure:
   - `quota`: Maximum requests per window (e.g., `1000`)
   - `window`: Time window in seconds (e.g., `60`)
3. Save the configuration

### Example: Configuring CORS

1. Add the `cors` plugin
2. Configure:
   - `allowed_origins`: Array of allowed origins (e.g., `["https://example.com"]`)
   - `allowed_methods`: HTTP methods (e.g., `["GET", "POST", "OPTIONS"]`)
   - `max_age`: Preflight cache duration in seconds
3. Save the configuration

## Builds

Compile your specs and plugins into deployable `.bca` artifacts.

### Triggering a Build

1. Navigate to a project's **Builds** tab
2. Click **Build**
3. Watch the compilation progress
4. Download the artifact when complete

### Build Status

| Status | Description |
|--------|-------------|
| **Pending** | Build queued |
| **Compiling** | Compilation in progress |
| **Succeeded** | Artifact ready for download/deploy |
| **Failed** | Check error details for issues |

### Build Errors

Common build errors:

- **Missing dispatcher** - Operation has no `x-barbacane-dispatch`
- **Invalid config** - Plugin configuration doesn't match schema
- **Plugin not found** - Referenced plugin not in registry
- **HTTP URL rejected** - Use HTTPS for production builds

## Deploy

Deploy artifacts to connected data planes for zero-downtime updates.

### Connecting Data Planes

Data planes connect to the control plane via WebSocket:

```bash
barbacane serve \
  --control-plane ws://localhost:9090/ws/data-plane \
  --project-id <project-uuid> \
  --api-key <api-key>
```

Connected data planes appear in the **Deploy** tab with status indicators:

| Status | Description |
|--------|-------------|
| **Online** | Connected and receiving updates |
| **Deploying** | Currently loading new artifact |
| **Offline** | Not connected |

### Creating API Keys

1. Navigate to a project's **Deploy** tab
2. Click **Create Key** in the API Keys section
3. Enter a descriptive name
4. **Copy the key immediately** - it's only shown once

### Deploying an Artifact

1. Ensure at least one data plane is connected
2. Click **Deploy Latest**
3. The control plane notifies all connected data planes
4. Data planes download, verify, and hot-reload the artifact

## Project Templates

Create new projects from templates using the **Init** page:

### Available Templates

| Template | Description |
|----------|-------------|
| **Basic** | Minimal OpenAPI spec with health endpoint |
| **Auth** | Includes JWT authentication middleware |
| **Full** | Complete example with auth, rate limiting, and CORS |

### Using Templates

1. Click **From Template** on the Projects page
2. Enter project name and select a template
3. Preview the generated files
4. Choose **Setup in Control Plane** to create project and upload spec
5. Or choose **Download** to get the files locally

## Settings

### Project Settings

Access from a project's **Settings** tab:

- **Rename project** - Update name and description
- **Production mode** - Enable/disable production compilation
- **Delete project** - Permanently remove project and all data

### Global Settings

Access from **Settings** in the sidebar:

- **Theme** - Light/dark mode toggle
- **API URL** - Control plane endpoint configuration

## Artifacts Browser

The **Artifacts** page shows all compiled artifacts:

- **Download** - Get `.bca` file for local deployment
- **View manifest** - See included specs and plugins
- **Delete** - Remove old artifacts

## Activity Log

The **Activity** page shows recent operations:

- Spec uploads
- Compilation jobs
- Deployments
- Plugin configuration changes

## Keyboard Shortcuts

| Shortcut | Action |
|----------|--------|
| `Ctrl/Cmd + K` | Quick search |
| `Esc` | Close dialogs |

## Troubleshooting

### UI won't load

1. Check that the control plane is running on port 9090
2. Verify the UI dev server is running on port 5173
3. Check browser console for errors

### Plugin configuration not saving

1. Verify the configuration matches the plugin's JSON Schema
2. Check for required fields that are empty
3. Look for validation errors in the form

### Data plane not appearing

1. Verify the data plane is started with correct flags
2. Check the API key is valid and not revoked
3. Ensure the project ID matches
4. Check WebSocket connection in data plane logs

### Build fails with "plugin not found"

1. Run `make seed-plugins` to populate the registry
2. Verify the plugin name in your spec matches the registry
3. Check the plugin version exists

## Next Steps

- [Control Plane API](control-plane.md) - Full REST API reference
- [CLI Reference](../reference/cli.md) - Command-line options
- [Plugin Development](../contributing/plugins.md) - Create custom plugins
