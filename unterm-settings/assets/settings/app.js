// Unterm Web Settings — Alpine.js controller.
//
// All `/api/*` requests carry the bootstrap auth token loaded once at
// startup from `/bootstrap.json`. Same-origin, no cookies, no CORS.
//
// i18n: a single dictionary is fetched at boot from `/api/i18n` and used by
// every `t(key)` call. When the user picks a different language the page
// reloads so all strings render in the new locale.

function untermSettings() {
  return {
    token: '',
    // Absolute path to the unterm-cli binary sibling of the running GUI,
    // injected by /bootstrap.json. Used by copyLaunchCmd / copyAuthCmd so
    // the copied command works even without the .app's MacOS dir on $PATH.
    untermCliPath: '',
    platform: 'unknown',
    health: { ok: false },
    state: {
      version: '',
      hostname: '',
      pid: '',
      started_at: '',
      ports: { mcp: '?', http: '?' },
      theme: 'standard',
      project: { path: '', slug: '' },
      sessions_path: '',
    },
    proxy: {
      enabled: false,
      mode: 'off',
      http_proxy: null,
      socks_proxy: null,
      no_proxy: '',
      health: null,
    },
    proxyForm: { http_proxy: '', socks_proxy: '', no_proxy: '' },
    newNode: { name: '', url: '' },
    clash: { connected: false, version: '', controller: '', groups: [] },
    clashGroup: '',
    clashCtl: { controller: '', secret: '' },
    nodeFilter: '',
    recording: { active: false },
    shells: {
      loading: false,
      loaded: false,
      busyId: null,
      defaultBusyId: null,
      defaultShell: null,
      list: [],
      platform: 'unknown',
      error: null,
    },

    // MCP trust + audit state. Backed by /api/mcp/*. Same lazy-load
    // pattern as profiles — only fetched when the user navigates to
    // the MCP tab. Two cards: trust list (runtime + static lua
    // config) and recent audit log (last N writes per agent).
    mcp: {
      loading: false,
      loaded: false,
      trusted: { runtime: [], static_config: [], audit_counts: [] },
      audit: [],
      newAgentInput: '',
    },

    // Providers tab — capability providers Unterm can reach, backed by
    // /api/providers/*. Lazy like the others, and deliberately: listing
    // providers reads a descriptor file, but *binding* wakes somebody's
    // browser, so nothing here contacts anything until asked.
    providers: {
      loading: false,
      loaded: false,
      error: null,
      list: [],
      leases: [],
      busy: null,
      diagnosis: null,
      approvals: [],
    },

    // Reference tab — live surface inventory from /api/reference, which
    // proxies the `meta.surface` MCP method. Loaded lazily on first visit.
    reference: {
      loading: false,
      loaded: false,
      error: null,
      filter: '',
      section: 'all',
      data: { version: '', mcp_methods: [], cli_commands: [], keybindings: [] },
    },

    // Identity profiles state. Backed by /api/profile/*. Loaded on first
    // visit to the Profiles tab (lazy — most users won't touch profiles).
    profiles: {
      loaded: false,
      loading: false,
      list: [],
      defaultId: null,
      sshInclude: { installed: false, user_config_path: '', include_path: '' },
      newForm: { open: false, display_name: '', accent_color: '#10b981' },
      expandedId: null,
      // Per-profile inline forms keyed by profile id. We keep them
      // distinct per profile rather than a single global so expanding
      // two cards doesn't crosswire their inputs.
      secretForms: {},    // id -> { env_name: '', value: '' }
      gitForms: {},       // id -> { user_name: '', user_email: '' }
      sshForms: {},       // id -> { host: '', key_path: '' }
    },

    // Onboarding wizard state. Used by both first-launch detection and
    // the manual "Run wizard" button. One-step UI: scan, present
    // candidates with checkboxes + paste boxes, click "Create".
    wizard: {
      active: false,
      loading: false,
      candidates: [],
      // Index → boolean. Default true for candidates with values, false
      // for those needing manual paste (avoids accidental empty submit).
      selected: {},
      manualValues: {},   // index → user-entered value for has_value=false candidates
      profileName: 'Personal',
      accentColor: '#10b981',
    },
    sessions: [],
    sessionMarkdown: null,
    currentSessionId: null,
    toasts: [],
    nextToast: 1,

    // i18n state
    lang: 'en-US',
    dict: {},
    availableLocales: [{ code: 'en-US', name: 'English' }],

    active: 'general',
    get nav() {
      return [
        { id: 'general', label: this.t('web.nav.general') },
        { id: 'shells', label: this.t('web.nav.shells') === 'web.nav.shells' ? 'Shells' : this.t('web.nav.shells') },
        { id: 'profiles', label: this.t('web.nav.profiles') },
        { id: 'agents', label: this.t('web.nav.agents'), badge: !this._agentsSeen },
        { id: 'review', label: this.t('web.nav.review'), badge: this.reviewBadge },
        { id: 'mcp', label: this.t('web.nav.mcp') },
        { id: 'appearance', label: this.t('web.nav.appearance') },
        { id: 'proxy', label: this.t('web.nav.proxy') },
        { id: 'providers', label: this.t('web.nav.providers') === 'web.nav.providers' ? 'Providers' : this.t('web.nav.providers') },
        { id: 'scrollback', label: this.t('web.nav.scrollback') },
        { id: 'compat', label: this.t('web.nav.compat') },
        { id: 'recording', label: this.t('web.nav.recording'), badge: !this._recordingSeen },
        { id: 'project', label: this.t('web.nav.project') },
        { id: 'reference', label: this.t('web.nav.reference') },
        { id: 'about', label: this.t('web.nav.about') },
      ];
    },
    _agentsSeen: false,
    _recordingSeen: false,

    // Scrollback config — number of lines kept in each pane's history
    // buffer. Existing panes keep their old buffer until they're closed,
    // because the per-pane VecDeque capacity is fixed at construction; we
    // surface that with `appliedAt` so the UI can show "restart to apply
    // to existing panes" right after Save.
    scrollback: {
      lines: 10000,
      default: 10000,
      min: 100,
      max: 999_999_999,
      saving: false,
      appliedAt: null,
    },

    // Compatibility config — what to advertise as TERM_PROGRAM into spawned
    // shells. Default "Unterm" keeps our brand identity; some third-party
    // tools (Gemini CLI, certain IDE detectors) only whitelist a fixed
    // set of terminal names, so users can spoof to dodge those checks.
    // `appliedAt` flips a hint asking the user to open a new tab — the
    // running shells keep their old TERM_PROGRAM until respawned.
    compat: {
      term_program: "Unterm",
      default: "Unterm",
      presets: ["Unterm", "WezTerm", "Apple_Terminal", "iTerm.app", "xterm"],
      saving: false,
      appliedAt: null,
    },

    // Update check state — populated from /api/updates which reads
    // ~/.unterm/update_check.json. The background poller writes that
    // file every 6 h; we just surface it. `dismissed` is a session-local
    // flag (sessionStorage) so the user can hush the banner for one
    // browser session without clobbering the underlying disk state —
    // next refresh / next deploy / next manual check brings it back.
    updates: {
      upgrade_available: false,
      latest_tag: "",
      current_pkg: "",
      checked_at: "",
      dismissed: false,
      checking: false,
      installing: false,
    },

    themes: [
      {
        id: 'agent-inbox',
        name: 'Agent Inbox',
        scheme: 'Agent Inbox',
        desc: "The inbox design's own deep ground with amber status accents",
        swatches: ['#16181d', '#d6d3cc', '#e8b34b', '#7aa2d6', '#6fb3a8'],
      },
      {
        id: 'standard',
        name: 'Standard',
        scheme: 'Unterm Dark',
        desc: 'Neutral high-contrast terminal style',
        swatches: ['#101010', '#f2f2f2', '#4fd6d6', '#5fd17a', '#5aa7ff'],
      },
      {
        id: 'midnight',
        name: 'Midnight',
        scheme: 'Unterm Midnight',
        desc: 'Low-glare blue-black workspace',
        swatches: ['#0f1420', '#e6edf7', '#72d6e8', '#8bdc88', '#82aaff'],
      },
      {
        id: 'daylight',
        name: 'Daylight',
        scheme: 'Unterm Daylight',
        desc: 'Readable light mode for bright rooms',
        swatches: ['#fbfbfa', '#0b0f14', '#005ea8', '#17643b', '#b42335'],
      },
      {
        id: 'classic',
        name: 'Classic',
        scheme: 'Classic Dark',
        desc: 'Plain high-contrast terminal colors',
        swatches: ['#121212', '#eeeeee', '#3b82f6', '#22c55e', '#ef4444'],
      },
      {
        id: 'notion-dark',
        name: 'Notion Dark',
        scheme: 'Notion Dark',
        desc: 'Notion-inspired warm dark',
        swatches: ['#181818', '#eeeeec', '#5aa7d6', '#4fb286', '#ff6f61'],
      },
      {
        id: 'notion-light',
        name: 'Notion Light',
        scheme: 'Notion Light',
        desc: 'Notion-inspired clean light',
        swatches: ['#f8f7f4', '#1f1e1a', '#1f6f9f', '#2f6f4f', '#b83232'],
      },
    ],

    /// Lookup helper. Returns the translated string for `key` or the key
    /// itself if the dictionary doesn't carry it.
    t(key) {
      const v = this.dict[key];
      return typeof v === 'string' ? v : key;
    },

    async boot() {
      try {
        const res = await fetch('/bootstrap.json');
        const j = await res.json();
        this.token = j.auth_token || '';
        this.untermCliPath = j.unterm_cli_path || '';
        this.platform = j.platform || 'unknown';
      } catch (e) {
        this.toast('Could not load bootstrap.json — backend offline?', 'error');
      }
      // Load i18n state before anything else so the rest of the boot path
      // can render translated text.
      await this.loadI18n();
      await this.refresh();
      const section = this.sectionFromHash();
      if (section && section !== this.active) this.select(section, false);
      window.addEventListener('hashchange', () => {
        const next = this.sectionFromHash();
        if (next && next !== this.active) this.select(next, false);
      });
      this.pollHealth();
      setInterval(() => this.pollHealth(), 5000);
      // Fleet metrics are sampled off-thread with a short TTL. Poll only
      // while Review is visible so the UI converges without background cost.
      setInterval(() => {
        if (this.active === 'review' && !this.review.busy) this.loadReview();
      }, 3500);
    },

    sectionFromHash() {
      const id = (window.location.hash || '').replace(/^#/, '').trim();
      if (!id) return null;
      return this.nav.some((item) => item.id === id) ? id : null;
    },

    async loadI18n() {
      try {
        const res = await fetch('/api/i18n', {
          headers: { Authorization: 'Bearer ' + this.token },
        });
        if (!res.ok) return;
        const j = await res.json();
        this.lang = j.current || 'en-US';
        this.dict = j.dict || {};
        this.availableLocales = j.available || this.availableLocales;
        document.documentElement.lang = this.lang;
        document.title = this.t('settings.title') || 'Unterm Settings';
      } catch (e) {
        // Fall through — t(key) returns the key itself when dict is empty.
      }
    },

    async changeLang(code) {
      try {
        await this.api('POST', '/api/i18n', { lang: code });
        this.toast(this.t('web.toast.lang_changed'));
        // Reload the SPA so every binding re-evaluates against the new dict.
        // We give the toast a moment to render before reloading.
        setTimeout(() => window.location.reload(), 250);
      } catch (e) {
        this.toast(this.t('web.toast.lang_failed').replace('{err}', e.message), 'error');
      }
    },

    async api(method, path, body) {
      const opts = {
        method,
        headers: {
          'Content-Type': 'application/json',
          Authorization: 'Bearer ' + this.token,
        },
      };
      if (body !== undefined) opts.body = JSON.stringify(body);
      const res = await fetch(path, opts);
      if (!res.ok) {
        let msg = res.status + ' ' + res.statusText;
        try { msg = (await res.json()).error || msg; } catch (e) {}
        throw new Error(msg);
      }
      const ct = res.headers.get('content-type') || '';
      if (ct.includes('application/json')) return res.json();
      return res.text();
    },

    async refresh() {
      try {
        const s = await this.api('GET', '/api/state');
        this.state = Object.assign({}, this.state, s);
        if (s.proxy) this.proxy = s.proxy;
        if (s.proxy) {
          this.proxyForm = {
            http_proxy: s.proxy.http_proxy || '',
            socks_proxy: s.proxy.socks_proxy || '',
            no_proxy: s.proxy.no_proxy || '',
          };
          // Read the Clash/mihomo controller so the rotation pool shows real
          // nodes to tick (non-blocking; updates this.clash reactively).
          this.loadClash();
        }
        if (s.recording) this.recording = s.recording;
        if (s.scrollback) {
          // Don't clobber `saving` / `appliedAt` UI flags — only sync the
          // numeric fields the server is the source of truth for.
          this.scrollback.lines = s.scrollback.lines ?? this.scrollback.lines;
          this.scrollback.default = s.scrollback.default ?? this.scrollback.default;
          this.scrollback.max = s.scrollback.max ?? this.scrollback.max;
        }
      } catch (e) {
        this.toast(this.t('web.toast.refresh').replace('{err}', e.message), 'error');
      }
      await this.loadSessions();
      await this.loadCompat();
      await this.loadUpdates();
    },

    async saveScrollback() {
      // Clamp client-side so we don't fire off requests that we know the
      // server will reject — keeps the round-trip cheap and the toast
      // friendlier than raw 400s.
      const n = Math.max(this.scrollback.min,
                Math.min(this.scrollback.max, Number(this.scrollback.lines) | 0));
      this.scrollback.lines = n;
      this.scrollback.saving = true;
      try {
        const j = await this.api('POST', '/api/scrollback', { lines: n });
        this.scrollback.appliedAt = new Date().toISOString();
        this.toast(this.t('web.toast.scrollback_saved').replace('{n}', String(j.lines)));
      } catch (e) {
        this.toast(this.t('web.toast.scrollback_failed').replace('{err}', e.message), 'error');
      } finally {
        this.scrollback.saving = false;
      }
    },

    resetScrollback() {
      this.scrollback.lines = this.scrollback.default;
      this.saveScrollback();
    },

    async loadCompat() {
      try {
        const j = await this.api('GET', '/api/compat');
        this.compat.term_program = j.term_program ?? this.compat.term_program;
        this.compat.default = j.default ?? this.compat.default;
        if (Array.isArray(j.presets)) this.compat.presets = j.presets;
      } catch (e) { /* leave defaults */ }
    },

    async saveCompat() {
      const value = (this.compat.term_program || '').trim() || this.compat.default;
      this.compat.term_program = value;
      this.compat.saving = true;
      try {
        const j = await this.api('POST', '/api/compat', { term_program: value });
        this.compat.appliedAt = new Date().toISOString();
        this.toast(this.t('web.toast.compat_saved').replace('{name}', j.term_program));
      } catch (e) {
        this.toast(this.t('web.toast.compat_failed').replace('{err}', e.message), 'error');
      } finally {
        this.compat.saving = false;
      }
    },

    pickCompatPreset(name) {
      this.compat.term_program = name;
      this.saveCompat();
    },

    resetCompat() {
      this.compat.term_program = this.compat.default;
      this.saveCompat();
    },

    async loadUpdates() {
      try {
        const j = await this.api('GET', '/api/updates');
        this.updates.upgrade_available = !!j.upgrade_available;
        this.updates.latest_tag = j.latest_tag ?? '';
        this.updates.current_pkg = j.current_pkg ?? '';
        this.updates.checked_at = j.checked_at ?? '';
        // Honor session-scoped dismissal (clicked × on the banner this
        // browser session). It resets on full reload — intentional, so
        // the user can't permanently silence themselves out of seeing
        // future versions.
        if (sessionStorage.getItem('unterm_update_dismissed') === this.updates.latest_tag) {
          this.updates.dismissed = true;
          this.updates.upgrade_available = false;
        }
      } catch (e) {
        // network blip on first load — leave defaults, don't toast spam
      }
    },

    async installUpdate() {
      this.updates.installing = true;
      try {
        const j = await this.api('POST', '/api/updates/install');
        if (j.status === 'scheduled') {
          this.toast(this.t('web.update.installing'));
        } else if (j.status === 'manual') {
          window.open(j.url, '_blank', 'noopener');
          this.updates.installing = false;
        } else {
          this.updates.installing = false;
          this.updates.upgrade_available = false;
        }
      } catch (e) {
        this.updates.installing = false;
        this.toast(String(e && e.message || e));
      }
    },

    async checkUpdatesNow() {
      this.updates.checking = true;
      try {
        const j = await this.api('POST', '/api/updates/check');
        this.updates.upgrade_available = !!j.upgrade_available;
        this.updates.latest_tag = j.latest_tag ?? '';
        this.updates.current_pkg = j.current_pkg ?? '';
        this.updates.checked_at = j.checked_at ?? '';
        this.updates.dismissed = false; // manual recheck unhushes
        sessionStorage.removeItem('unterm_update_dismissed');
        const msg = this.updates.upgrade_available
          ? this.t('web.toast.update_available').replace('{tag}', this.updates.latest_tag)
          : this.t('web.toast.update_uptodate');
        this.toast(msg);
      } catch (e) {
        this.toast(this.t('web.toast.update_failed').replace('{err}', e.message), 'error');
      } finally {
        this.updates.checking = false;
      }
    },

    dismissUpdate() {
      this.updates.dismissed = true;
      // Pin the dismissal to the specific tag — if a yet-newer version
      // arrives later, the banner re-emerges.
      sessionStorage.setItem('unterm_update_dismissed', this.updates.latest_tag);
      this.updates.upgrade_available = false;
    },

    async pollHealth() {
      try {
        const j = await this.api('GET', '/api/health');
        this.health = { ok: !!j.ok };
      } catch (e) {
        this.health = { ok: false };
      }
    },

    async loadSessions() {
      try {
        const j = await this.api('GET', '/api/sessions');
        this.sessions = (j.sessions || []).slice().reverse();
      } catch (e) {
        this.sessions = [];
      }
    },

    // --- Agent Cockpit: Review page ---
    review: { fleets: [], checkpoints: [], sel: null, diff: null, busy: false, verifying: {}, error: '', compareFleet: null, cmpA: null, cmpB: null, loadedAt: 0 },
    _reviewRowsCache: new Map(),
    get reviewBadge() {
      return this.review.fleets.some((f) =>
        (f.members || []).some((m) => m.review === 'pending')
      );
    },
    get reviewStats() {
      const members = this.review.fleets.flatMap((f) => f.members || []);
      return {
        active: members.filter((m) => m.agent_state === 'working').length,
        waiting: members.filter((m) => m.agent_state === 'waiting').length,
        pending: members.filter((m) => m.review === 'pending').length,
        checkpoints: this.review.checkpoints.reduce((n, r) => n + (r.checkpoints || []).length, 0),
      };
    },
    reviewAge(iso) {
      const ms = Date.now() - Date.parse(iso || '');
      if (!Number.isFinite(ms) || ms < 0) return '';
      const mins = Math.floor(ms / 60000);
      if (mins < 1) return 'now';
      if (mins < 60) return `${mins}m`;
      const hours = Math.floor(mins / 60);
      return hours < 24 ? `${hours}h` : `${Math.floor(hours / 24)}d`;
    },
    reviewDuration(seconds) {
      const value = Number(seconds);
      if (!Number.isFinite(value)) return '';
      if (value < 60) return `${Math.max(0, Math.floor(value))}s`;
      if (value < 3600) return `${Math.floor(value / 60)}m`;
      return `${Math.floor(value / 3600)}h ${Math.floor((value % 3600) / 60)}m`;
    },
    reviewVerification(member) {
      return (member && member.verification) || {};
    },
    reviewVerificationStatus(member) {
      const status = String(this.reviewVerification(member).status || 'not_run').toLowerCase();
      if (status === 'success') return 'passed';
      if (status === 'failure') return 'failed';
      if (status === 'pending') return 'queued';
      if (status === 'timed_out') return 'timeout';
      return status;
    },
    reviewVerificationLabel(member) {
      const status = this.reviewVerificationStatus(member);
      const known = ['passed', 'failed', 'running', 'queued', 'timeout', 'error', 'not_run'];
      return this.t(`web.review.verify_${known.includes(status) ? status : 'not_run'}`);
    },
    reviewVerificationDuration(member) {
      const ms = Number(this.reviewVerification(member).duration_ms);
      if (!Number.isFinite(ms) || ms < 0) return '';
      if (ms < 1000) return `${Math.floor(ms)}ms`;
      return this.reviewDuration(ms / 1000);
    },
    reviewVerificationSummary(member) {
      const verification = this.reviewVerification(member);
      const summary = String(verification.summary || verification.log || '').trim();
      if (summary.length <= 420) return summary;
      return `…${summary.slice(-419)}`;
    },
    reviewMember(fleetId, member) {
      const fleet = this.review.fleets.find((item) => String(item.id) === String(fleetId));
      return fleet && (fleet.members || []).find((item) => String(item.n) === String(member));
    },
    reviewVerifying(fleetId, member) {
      return !!this.review.verifying[`${fleetId}:${member}`];
    },
    async reviewVerify(fleetId, member, retry = false) {
      const item = this.reviewMember(fleetId, member) || {};
      const verification = this.reviewVerification(item);
      const command = String(verification.command || '').trim();
      const key = `${fleetId}:${member}`;
      this.review.verifying = { ...this.review.verifying, [key]: true };
      try {
        const body = {
          fleet_id: fleetId,
          member: String(member),
        };
        if (command) body.command = command;
        await this.api('POST', retry ? '/api/review/retry' : '/api/review/verify', body);
        await this.loadReview();
        this.review.error = '';
      } catch (e) {
        this.review.error = String(e);
      } finally {
        const next = { ...this.review.verifying };
        delete next[key];
        this.review.verifying = next;
      }
    },
    async reviewRetryAgent(fleetId, member) {
      const key = `${fleetId}:${member}`;
      this.review.verifying = { ...this.review.verifying, [key]: true };
      try {
        await this.api('POST', '/api/fleet/retry', {
          fleet_id: fleetId,
          member: String(member),
        });
        await this.loadReview();
        this.review.error = '';
      } catch (e) {
        this.review.error = String(e);
      } finally {
        const next = { ...this.review.verifying };
        delete next[key];
        this.review.verifying = next;
      }
    },
    async loadReview() {
      try {
        const data = await this.api('GET', '/api/review/overview');
        this.review.fleets = data.fleets || [];
        this.review.checkpoints = data.checkpoints || [];
        this.review.loadedAt = Date.now();
        this.review.error = '';
      } catch (e) {
        this.review.error = String(e);
      }
    },
    async reviewSelect(kind, a, b) {
      // kind: 'member' (fleet id, member n) | 'checkpoint' (repo, sha)
      this.review.sel = { kind, a, b };
      this.review.diff = null;
      this.review.busy = true;
      try {
        const q =
          kind === 'member'
            ? `fleet=${encodeURIComponent(a)}&member=${encodeURIComponent(b)}`
            : `repo=${encodeURIComponent(a)}&from=${encodeURIComponent(b)}`;
        this.review.diff = await this.api('GET', '/api/review/diff?' + q);
        this.review.error = '';
      } catch (e) {
        this.review.error = String(e);
      } finally {
        this.review.busy = false;
      }
    },
    reviewDiffRows() {
      return this.reviewRowsFor(this.review.diff);
    },
    reviewToggleCompare(fleetId) {
      if (this.review.compareFleet === fleetId) {
        this.review.compareFleet = null;
        this.review.cmpA = null;
        this.review.cmpB = null;
      } else {
        this.review.compareFleet = fleetId;
        this.review.cmpA = null;
        this.review.cmpB = null;
        this.review.sel = null;
        this.review.diff = null;
      }
    },
    async reviewComparePick(fleetId, member) {
      if (this.review.compareFleet !== fleetId) return;
      this.review.busy = true;
      try {
        const q = `fleet=${encodeURIComponent(fleetId)}&member=${encodeURIComponent(member)}`;
        const diff = await this.api('GET', '/api/review/diff?' + q);
        const fleet = this.review.fleets.find((f) => f.id === fleetId);
        const meta = (fleet && (fleet.members || []).find((m) => String(m.n) === String(member))) || {};
        const slot = { member, diff, agent: meta.agent || 'agent', branch: meta.branch || '' };
        if (!this.review.cmpA || (this.review.cmpA && this.review.cmpB)) {
          this.review.cmpA = slot;
          this.review.cmpB = null;
        } else if (this.review.cmpA.member !== member) {
          this.review.cmpB = slot;
        }
        this.review.error = '';
      } catch (e) {
        this.review.error = String(e);
      } finally {
        this.review.busy = false;
      }
    },
    reviewRowsFor(diff) {
      const patch = (diff && diff.patch) || '';
      const cached = this._reviewRowsCache.get(patch);
      if (cached) return cached;
      const rows = [];
      for (const line of patch.split('\n')) {
        let cls = 'text-notion-muted';
        if (line.startsWith('+++') || line.startsWith('---')) cls = 'text-notion-ink font-semibold';
        else if (line.startsWith('diff --git')) cls = 'mt-4 text-notion-teal font-semibold';
        else if (line.startsWith('@@')) cls = 'text-sky-400';
        else if (line.startsWith('+')) cls = 'text-emerald-400 bg-emerald-950/40';
        else if (line.startsWith('-')) cls = 'text-rose-400 bg-rose-950/40';
        rows.push({ text: line, cls });
        if (rows.length > 2500) {
          rows.push({ text: '… (truncated)', cls: 'text-notion-muted italic' });
          break;
        }
      }
      // Bound memory while avoiding repeated parsing during Alpine redraws.
      if (this._reviewRowsCache.size >= 8) {
        this._reviewRowsCache.delete(this._reviewRowsCache.keys().next().value);
      }
      this._reviewRowsCache.set(patch, rows);
      return rows;
    },
    async reviewMerge(fleetId, member) {
      if (!window.confirm(`${this.t('web.review.merge')}\n\n${fleetId} / #${member}`)) return;
      this.review.busy = true;
      try {
        await this.api('POST', '/api/review/merge', { fleet_id: fleetId, member: String(member) });
        await this.loadReview();
      } catch (e) {
        this.review.error = String(e);
      } finally {
        this.review.busy = false;
      }
    },
    async reviewDiscard(fleetId, member) {
      if (!window.confirm(`${this.t('web.review.discard')}\n\n${fleetId} / #${member}`)) return;
      this.review.busy = true;
      try {
        await this.api('POST', '/api/review/discard', { fleet_id: fleetId, member: String(member) });
        await this.loadReview();
      } catch (e) {
        this.review.error = String(e);
      } finally {
        this.review.busy = false;
      }
    },
    async reviewClean(fleetId) {
      if (!window.confirm(`${this.t('web.review.clean')}\n\n${fleetId}`)) return;
      this.review.busy = true;
      try {
        await this.api('POST', '/api/review/clean', { id: fleetId });
        this.review.sel = null;
        this.review.diff = null;
        await this.loadReview();
      } catch (e) {
        this.review.error = String(e);
      } finally {
        this.review.busy = false;
      }
    },
    async reviewRollback(repo, sha) {
      const msg = this.t('web.review.rollback_confirm')
        .replace('{repo}', repo)
        .replace('{sha}', sha.slice(0, 12));
      if (!window.confirm(msg)) return;
      this.review.busy = true;
      try {
        await this.api('POST', '/api/review/rollback', { repo, sha });
        await this.loadReview();
        this.review.diff = null;
        this.review.sel = null;
      } catch (e) {
        this.review.error = String(e);
      } finally {
        this.review.busy = false;
      }
    },

    async loadProviders(rediscover = false) {
      this.providers.loading = true;
      this.providers.error = null;
      try {
        const suffix = rediscover ? '?rediscover=1' : '';
        const res = await fetch('/api/providers' + suffix, {
          headers: { Authorization: 'Bearer ' + this.token },
        });
        const data = await res.json();
        if (!res.ok) throw new Error(data.error || res.statusText);
        this.providers.list = data.providers || [];
        // What is waiting on the person reading this page. Fetched with the
        // provider list because that is where they will be looking when an
        // agent asks for their browser.
        const askRes = await fetch('/api/approvals', {
          headers: { Authorization: 'Bearer ' + this.token },
        });
        const askData = await askRes.json();
        this.providers.approvals = askData.approvals || [];
        const leaseRes = await fetch('/api/providers/leases', {
          headers: { Authorization: 'Bearer ' + this.token },
        });
        const leaseData = await leaseRes.json();
        this.providers.leases = leaseData.leases || [];
        this.providers.loaded = true;
      } catch (e) {
        // Shown, not swallowed: "no providers" and "could not look" are
        // different answers and only one means install something.
        this.providers.error = String(e.message || e);
      } finally {
        this.providers.loading = false;
      }
    },

    async providerAction(id, action, body = {}) {
      this.providers.busy = id + ':' + action;
      this.providers.error = null;
      this.providers.diagnosis = null;
      try {
        const res = await fetch('/api/providers/' + action, {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            Authorization: 'Bearer ' + this.token,
          },
          body: JSON.stringify({ provider: id, ...body }),
        });
        const data = await res.json();
        if (!res.ok) throw new Error(data.error || res.statusText);
        if (action === 'diagnose') this.providers.diagnosis = data;
      } catch (e) {
        this.providers.error = String(e.message || e);
      } finally {
        this.providers.busy = null;
        await this.loadProviders();
      }
    },

    async decideApproval(id, allowed, remember = null) {
      this.providers.busy = 'approval:' + id;
      try {
        const res = await fetch('/api/approvals/decide', {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            Authorization: 'Bearer ' + this.token,
          },
          body: JSON.stringify({ approval: id, allowed, remember }),
        });
        const data = await res.json();
        if (!res.ok) throw new Error(data.error || res.statusText);
      } catch (e) {
        this.providers.error = String(e.message || e);
      } finally {
        this.providers.busy = null;
        await this.loadProviders();
      }
    },

    async revokeLease(id) {
      this.providers.busy = 'lease:' + id;
      try {
        const res = await fetch('/api/providers/leases/revoke', {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            Authorization: 'Bearer ' + this.token,
          },
          body: JSON.stringify({ lease: id }),
        });
        const data = await res.json();
        if (!res.ok) throw new Error(data.error || res.statusText);
      } catch (e) {
        this.providers.error = String(e.message || e);
      } finally {
        this.providers.busy = null;
        await this.loadProviders();
      }
    },

    providerTone(state) {
      if (state === 'ready') return 'text-notion-teal';
      if (state === 'paused') return 'text-notion-muted';
      if (state === 'degraded') return 'text-orange-400';
      return 'text-notion-faint';
    },

    select(id, updateHash = true) {
      this.active = id;
      if (id === 'review') this.loadReview();
      if (updateHash && window.location.hash !== '#' + id) {
        window.history.replaceState(null, '', '#' + id);
      }
      if (id === 'recording') this._recordingSeen = true;
      if (id === 'agents') this._agentsSeen = true;
      // Lazy-load profiles on first visit so users who never touch the
      // tab don't pay for the registry-load + sniffer scan.
      if (id === 'profiles' && !this.profiles.loaded) this.loadProfiles();
      if (id === 'mcp' && !this.mcp.loaded) this.loadMcp();
      if (id === 'reference' && !this.reference.loaded) this.loadReference();
      if (id === 'providers' && !this.providers.loaded) this.loadProviders();
      if (id === 'shells') this.loadShells();
      if (id === 'agents') {
        // Re-detect every time the tab is opened, not just the first time.
        // Otherwise a binary the user installs in a shell side-by-side won't
        // show up here without a full panel reload, and worse: a stale "not
        // installed" can lead them to click Install for something that's
        // already on PATH (we hit this on 2026-05-20 with Claude Code).
        this.loadAgents();
      }
    },

    async loadShells() {
      this.shells.loading = true;
      this.shells.error = null;
      try {
        const res = await this.api('GET', '/api/shells');
        this.shells.platform = res.platform || this.platform || 'unknown';
        this.shells.list = res.shells || [];
        this.shells.defaultShell = res.default_shell || null;
        this.shells.loaded = true;
      } catch (e) {
        this.shells.error = e.message;
        this.toast('Shell detection failed: ' + e.message, 'error');
      } finally {
        this.shells.loading = false;
      }
    },

    async useDefaultShell(shell) {
      if (!shell || !shell.id || !shell.installed) return;
      this.shells.defaultBusyId = shell.id;
      try {
        const res = await this.api('POST', '/api/shells/default', { id: shell.id });
        this.shells.defaultShell = res.default_shell || null;
        this.toast(shell.name + ' will be used for new shells', 'success');
      } catch (e) {
        this.toast('Default shell update failed: ' + e.message, 'error');
      } finally {
        this.shells.defaultBusyId = null;
      }
    },

    isDefaultShell(shell) {
      const argv = this.shells.defaultShell?.argv || [];
      const current = (argv[0] || '').toLowerCase().replaceAll('\\', '/');
      const path = (shell.path || '').toLowerCase().replaceAll('\\', '/');
      if (!current || !path) return false;
      return current === path || current.endsWith('/' + path.split('/').pop());
    },

    copyShellInstall(shell) {
      if (!shell || !shell.install_command) return;
      this.copyText(shell.install_command);
      this.toast('Install command copied', 'info');
    },

    async installShell(shell) {
      if (!shell || !shell.id || !shell.install_command) return;
      if (!window.confirm('Install ' + shell.name + ' by running: ' + shell.install_command + '?')) return;
      this.shells.busyId = shell.id;
      try {
        const res = await this.api('POST', '/api/shells/install', { id: shell.id });
        this.toast('Installer started: ' + (res.command || shell.install_command), 'success');
        setTimeout(() => this.loadShells(), 1500);
      } catch (e) {
        this.toast('Install failed: ' + e.message, 'error');
      } finally {
        this.shells.busyId = null;
      }
    },

    // ---------- AI Agents tab ----------
    //
    // Backed entirely by /api/agents/* routes (see web_settings/agents.rs).
    // The settings form is rendered from the manifest's `settings_schema`
    // array so adding a new agent or knob doesn't require an SPA change.
    //
    // State machine:
    //   agents.list        ←  GET /api/agents/list  (one row per manifest)
    //   agents.detail      ←  GET /api/agents/<id>/settings  (when opened)
    //   agents.detail.draft is a live-edited copy of values; we POST
    //     PUT /api/agents/<id>/settings only on Save, so partial typing
    //     doesn't keep landing on disk.
    //   agents.profileId   ←  identity profile to scope settings + secrets.
    agents: {
      loaded: false,
      loading: false,
      list: [],
      profiles: [],
      profileId: 'default',
      envelope: null,
      detail: null,
      error: null,
      busyId: null,
    },

    async loadAgents() {
      this.agents.loading = true;
      this.agents.error = null;
      try {
        // Pull the identity profile list so the selector has options. We
        // reuse the existing /api/profile/list route; failure is non-fatal
        // (we still show 'default').
        if (this.agents.profiles.length === 0) {
          try {
            const p = await this.api('GET', '/api/profile/list');
            const list = (p.profiles || []).map((x) => ({ id: x.id, display_name: x.display_name }));
            this.agents.profiles = [{ id: 'default', display_name: this.t('web.agents.profile.default') }].concat(list.filter((x) => x.id !== 'default'));
          } catch (_) {
            this.agents.profiles = [{ id: 'default', display_name: 'default' }];
          }
        }
        const res = await this.api('GET', '/api/agents/list');
        this.agents.envelope = {
          envelope_source: res.envelope_source,
          envelope_issued_at: res.envelope_issued_at,
          envelope_expires_at: res.envelope_expires_at,
          signing_key_id: res.signing_key_id,
        };
        this.agents.list = res.agents || [];
        this.agents.loaded = true;
      } catch (e) {
        this.agents.error = e.message;
      } finally {
        this.agents.loading = false;
      }
    },

    async refreshAgents() {
      try {
        await this.api('POST', '/api/agents/manifest/refresh');
      } catch (_) {}
      this.agents.loaded = false;
      this.agents.detail = null;
      await this.loadAgents();
      this.toast(this.t('web.agents.toast.refreshed'), 'info');
    },

    async openAgent(id) {
      try {
        const detail = await this.api(
          'GET',
          '/api/agents/' + encodeURIComponent(id) + '/settings?profile=' + encodeURIComponent(this.agents.profileId),
        );
        // Build a draft copy. Secret-typed settings come back as
        // {_secret, is_set}; we keep them in `values` (for the placeholder)
        // but seed the draft with an empty string so typing replaces.
        const draft = {};
        for (const s of detail.schema || []) {
          const v = (detail.values || {})[s.key];
          if (s.type === 'secret') {
            draft[s.key] = '';
          } else {
            draft[s.key] = v !== undefined ? this.deepClone(v) : s.default;
          }
        }
        // Seed the auth_mode selection. Order: existing stored value →
        // recommended mode → first declared mode → empty string.
        const manifestForMode = detail.manifest;
        const modes = (manifestForMode && manifestForMode.auth_modes) || [];
        const storedMode = (detail.values && detail.values._auth_mode) || '';
        const defaultMode =
          (modes.length && (modes.find((m) => m.recommended) || modes[0]).id) || '';
        draft._auth_mode = storedMode || defaultMode;
        // Seed launch-flag selection from the saved _launch_flags, pre-creating
        // an entry for every catalog flag so the Alpine x-model bindings are
        // reactive from the first render (toggles -> bool, value/choice -> str).
        const savedFlags = (detail.values && detail.values._launch_flags) || {};
        const flagCatalog = (manifestForMode && manifestForMode.launch && manifestForMode.launch.flag_catalog) || [];
        draft._launch_flags = {};
        for (const f of flagCatalog) {
          const sv = savedFlags[f.id];
          draft._launch_flags[f.id] = f.kind === 'toggle'
            ? (sv === true)
            : (typeof sv === 'string' ? sv : '');
        }
        const categories = Array.from(new Set((detail.schema || []).map((s) => s.category || 'general')));
        this.agents.detail = {
          manifest: detail.manifest || (await this.api('GET', '/api/agents/' + encodeURIComponent(id))).manifest,
          // Detect comes back inline now (the /settings handler re-runs it on
          // every open). Older builds didn't include it; default to a not-ok
          // shape so the card just shows "not installed" rather than blowing
          // up on undefined access.
          detect: detail.detect || { ok: false, version: null, binary_path: null },
          schema: detail.schema || [],
          values: detail.values || {},
          headless_supported: detail.headless_supported === true,
          headless_default_prompt: detail.headless_default_prompt || null,
          categories,
          draft,
          dirty: false,
          saving: false,
        };
        // Re-fetch manifest for the storage paths + categories — the
        // /settings response includes a flat schema but not full storage.
        if (!this.agents.detail.manifest) {
          const m = await this.api('GET', '/api/agents/' + encodeURIComponent(id));
          this.agents.detail.manifest = m.manifest;
        }
        // Watch draft for dirty flag.
        this.$watch('agents.detail.draft', () => {
          if (this.agents.detail) this.agents.detail.dirty = true;
        }, { deep: true });
      } catch (e) {
        this.toast(this.t('web.agents.toast.load_failed').replace('{err}', e.message), 'error');
      }
    },

    deepClone(v) {
      return JSON.parse(JSON.stringify(v));
    },

    // Whether a settings_schema entry should be shown given the current
    // auth_mode selection. Rules:
    //   * Settings in non-auth categories are always visible (model,
    //     behavior, permissions, privacy, etc. — they're orthogonal to
    //     who pays for the calls).
    //   * Settings in the "auth" category are visible only if the
    //     currently-selected auth_mode lists their key in reveals_settings.
    //   * If no auth_modes are declared (legacy v0.18.0 manifests), we
    //     fall back to showing all settings — keeps the panel usable
    //     across mixed manifest versions.
    specVisibleInMode(spec) {
      const modes = this.agents.detail?.manifest?.auth_modes || [];
      if (!modes.length) return true;
      const cat = spec.category || 'general';
      if (cat !== 'auth') return true;
      const currentId = this.agents.detail?.draft?._auth_mode;
      const current = modes.find((m) => m.id === currentId) || modes.find((m) => m.recommended) || modes[0];
      return (current?.reveals_settings || []).includes(spec.key);
    },

    toggleAgentMultiEnum(key, value, checked) {
      const cur = Array.isArray(this.agents.detail.draft[key]) ? this.agents.detail.draft[key].slice() : [];
      const idx = cur.indexOf(value);
      if (checked && idx < 0) cur.push(value);
      if (!checked && idx >= 0) cur.splice(idx, 1);
      this.agents.detail.draft[key] = cur;
      this.agents.detail.dirty = true;
    },

    resetAgentDraft() {
      if (!this.agents.detail) return;
      const schema = this.agents.detail.schema || [];
      const values = this.agents.detail.values || {};
      for (const s of schema) {
        if (s.type === 'secret') {
          this.agents.detail.draft[s.key] = '';
        } else {
          const v = values[s.key];
          this.agents.detail.draft[s.key] = v !== undefined ? this.deepClone(v) : s.default;
        }
      }
      this.agents.detail.dirty = false;
    },

    async saveAgent() {
      if (!this.agents.detail) return;
      this.agents.detail.saving = true;
      try {
        const values = {};
        // Always persist the auth_mode selection so the launcher reads
        // back the right mode next time the user spawns this agent.
        if (this.agents.detail.draft._auth_mode) {
          values._auth_mode = this.agents.detail.draft._auth_mode;
        }
        // Persist the launch-flag selection (synthetic key — stays in Unterm's
        // per-profile state, never written to the agent's own config file).
        if (this.agents.detail.draft._launch_flags) {
          values._launch_flags = this.agents.detail.draft._launch_flags;
        }
        for (const s of this.agents.detail.schema) {
          const d = this.agents.detail.draft[s.key];
          // Skip empty secret fields — preserves whatever's in the keychain.
          if (s.type === 'secret') {
            if (d === '' || d == null) continue;
            values[s.key] = d;
            continue;
          }
          values[s.key] = d;
        }
        const res = await this.api(
          'PUT',
          '/api/agents/' + encodeURIComponent(this.agents.detail.manifest.id) + '/settings',
          { profile: this.agents.profileId, values },
        );
        this.toast(this.t('web.agents.toast.saved').replace('{n}', String(res.written_files?.length || 0)), 'success');
        // Re-open to reflect what landed on disk (incl. preserved-unknown-keys).
        await this.openAgent(this.agents.detail.manifest.id);
      } catch (e) {
        this.toast(this.t('web.agents.toast.save_failed').replace('{err}', e.message), 'error');
      } finally {
        if (this.agents.detail) this.agents.detail.saving = false;
      }
    },

    async installAgent(id) {
      this.agents.busyId = id;
      // Pre-check: re-detect right now before we shell out to npm/pipx.
      // The list view's `installed` flag could be from an earlier page load
      // when PATH was incomplete (macOS Finder-launch problem). If we now
      // see it on PATH, skip install and surface "already installed."
      try {
        const fresh = await this.api('GET', '/api/agents/' + encodeURIComponent(id));
        if (fresh && fresh.detect && fresh.detect.ok) {
          this.toast(this.t('web.agents.toast.already_installed').replace('{v}', fresh.detect.version || '?'), 'info');
          await this.loadAgents();
          this.agents.busyId = null;
          return;
        }
      } catch (_) { /* fall through to actual install */ }
      try {
        const res = await this.api('POST', '/api/agents/' + encodeURIComponent(id) + '/install');
        if (res.ok) {
          this.toast(this.t('web.agents.toast.installed'), 'success');
          await this.loadAgents();
        } else {
          this.toast(this.t('web.agents.toast.install_failed'), 'error');
        }
      } catch (e) {
        this.toast(this.t('web.agents.toast.install_failed') + ': ' + e.message, 'error');
      } finally {
        this.agents.busyId = null;
      }
    },

    async uninstallAgent(id) {
      if (!confirm(this.t('web.agents.confirm.uninstall'))) return;
      try {
        await this.api('POST', '/api/agents/' + encodeURIComponent(id) + '/uninstall');
        this.toast(this.t('web.agents.toast.uninstalled'), 'success');
        this.agents.detail = null;
        await this.loadAgents();
      } catch (e) {
        this.toast(e.message, 'error');
      }
    },

    async importAgent(id) {
      try {
        const res = await this.api(
          'GET',
          '/api/agents/' + encodeURIComponent(id) + '/import?profile=' + encodeURIComponent(this.agents.profileId),
        );
        const count = Object.keys(res.imported || {}).length;
        if (count === 0) {
          this.toast(this.t('web.agents.toast.import_empty'), 'info');
          return;
        }
        // Merge into the current draft so the user sees the values before saving.
        for (const [k, v] of Object.entries(res.imported)) {
          if (v && typeof v === 'object' && v._secret) continue;
          this.agents.detail.draft[k] = v;
        }
        this.agents.detail.dirty = true;
        this.toast(this.t('web.agents.toast.imported').replace('{n}', String(count)), 'success');
      } catch (e) {
        this.toast(e.message, 'error');
      }
    },

    async clearSecret(key) {
      // Erase the keychain entry by PUT-ing an explicit empty value. The
      // settings_storage adapter doesn't know about secrets — they go
      // through the secret_store layer; sending value '' tells it to
      // remove the entry server-side (see registry::apply_updates).
      try {
        await this.api(
          'PUT',
          '/api/agents/' + encodeURIComponent(this.agents.detail.manifest.id) + '/settings',
          { profile: this.agents.profileId, values: { [key]: '' } },
        );
        await this.openAgent(this.agents.detail.manifest.id);
        this.toast(this.t('web.agents.toast.secret_cleared'), 'info');
      } catch (e) {
        this.toast(e.message, 'error');
      }
    },

    // Build the unterm-cli command prefix using the absolute path the
    // backend told us about at bootstrap. Falls back to bare "unterm-cli"
    // if the field is missing (older Unterm builds). Shell-quote the path
    // because the .app bundle on macOS lives under /Applications/Unterm.app/…
    // which has no spaces today, but if a user renames the app or installs
    // to a path with spaces we don't want the copied command to break.
    _cliPrefix() {
      const p = this.untermCliPath || 'unterm-cli';
      if (p === 'unterm-cli') return p;
      return this.commandQuote(p);
    },

    shellQuote(s) {
      return "'" + String(s).replace(/'/g, "'\\''") + "'";
    },

    cmdQuote(s) {
      const raw = String(s);
      if (raw.length === 0) return '""';
      if (/^[A-Za-z0-9_./\\:=-]+$/.test(raw)) return raw;
      return '"' + raw.replace(/(\\*)"/g, '$1$1\\"').replace(/\\+$/g, '$&$&') + '"';
    },

    commandQuote(s) {
      return this.platform === 'windows' ? this.cmdQuote(s) : this.shellQuote(s);
    },

    agentSummary(id) {
      return (this.agents.list || []).find((a) => a.id === id) || null;
    },

    supportsHeadlessAgent(id) {
      if (this.agents.detail?.manifest?.id === id
          && this.agents.detail.headless_supported !== undefined) {
        return this.agents.detail.headless_supported === true;
      }
      const summary = this.agentSummary(id);
      if (summary && summary.headless_supported !== undefined) {
        return summary.headless_supported === true;
      }
      return id === 'codex-cli'
        || id === 'claude-code'
        || id === 'gemini-cli'
        || id === 'opencode'
        || id === 'kimi-code'
        || id === 'trae-agent';
    },

    defaultHeadlessPrompt(id) {
      if (this.agents.detail?.manifest?.id === id
          && this.agents.detail.headless_default_prompt) {
        return this.agents.detail.headless_default_prompt;
      }
      const summary = this.agentSummary(id);
      if (summary?.headless_default_prompt) return summary.headless_default_prompt;
      if (id === 'codex-cli') return 'review this diff and list risky changes';
      if (id === 'claude-code') return 'summarise the last failing test output';
      if (id === 'gemini-cli') return 'summarise this repository and suggest the next useful task';
      if (id === 'opencode') return 'inspect the current project and suggest the next useful task';
      if (id === 'kimi-code') return 'inspect this project and suggest the next useful task';
      if (id === 'trae-agent') return 'inspect this project and suggest the next useful task';
      return 'summarise the current task';
    },

    copyLaunchCmd(id) {
      const cmd = this._cliPrefix() + ' agent launch ' + id + ' --profile ' + this.agents.profileId;
      this.copyText(cmd);
      this.toast(this.t('web.agents.toast.launch_copied'), 'info');
    },

    async copyRunCmd(id) {
      const placeholder = this.defaultHeadlessPrompt(id);
      let cmd = '';
      try {
        const plan = await this.api(
          'POST',
          '/api/agents/' + encodeURIComponent(id) + '/run-plan',
          { profile: this.agents.profileId, prompt: placeholder },
        );
        cmd = plan.command || '';
      } catch (_) {}
      if (!cmd) {
        cmd = this._cliPrefix()
          + ' agent run ' + id
          + ' --profile ' + this.agents.profileId
          + ' ' + this.commandQuote(placeholder);
      }
      this.copyText(cmd);
      this.toast(this.t('web.agents.toast.run_copied'), 'info');
    },

    copyAuthCmd(id) {
      const cmd = this._cliPrefix() + ' agent auth ' + id + ' --profile ' + this.agents.profileId;
      this.copyText(cmd);
      this.toast(this.t('web.agents.toast.auth_copied'), 'info');
    },

    copyText(s) {
      try {
        navigator.clipboard.writeText(s);
      } catch (_) {
        // Older browsers / non-https: degrade to a select-and-copy.
        const ta = document.createElement('textarea');
        ta.value = s;
        document.body.appendChild(ta);
        ta.select();
        try { document.execCommand('copy'); } catch (_) {}
        document.body.removeChild(ta);
      }
    },

    async loadReference() {
      this.reference.loading = true;
      this.reference.error = null;
      try {
        const data = await this.api('GET', '/api/reference');
        this.reference.data = data;
        this.reference.loaded = true;
      } catch (e) {
        this.reference.error = e.message || String(e);
      } finally {
        this.reference.loading = false;
      }
    },

    filteredMcp() {
      const items = this.reference.data.mcp_methods || [];
      const f = (this.reference.filter || '').toLowerCase();
      if (!f) return items;
      return items.filter((m) =>
        (m.name + ' ' + (m.summary || '') + ' ' + (m.namespace || ''))
          .toLowerCase()
          .includes(f)
      );
    },

    filteredCli() {
      const items = this.reference.data.cli_commands || [];
      const f = (this.reference.filter || '').toLowerCase();
      if (!f) return items;
      return items.filter((c) =>
        (c.name + ' ' + (c.summary || '') + ' ' + ((c.subcommands || []).join(' ')))
          .toLowerCase()
          .includes(f)
      );
    },

    filteredKeys() {
      const items = this.reference.data.keybindings || [];
      const f = (this.reference.filter || '').toLowerCase();
      if (!f) return items;
      return items.filter((k) =>
        (this.formatKey(k) + ' ' + (k.action || '') + ' ' + (k.table || ''))
          .toLowerCase()
          .includes(f)
      );
    },

    formatKey(k) {
      // mods are formatted by the server as bitflags-debug like `CTRL | SHIFT`
      // or `NONE`. Clean those up for display.
      const m = (k.mods || '').replace(/\s*\|\s*/g, '+').replace(/^NONE$/i, '');
      const key = k.key || '';
      return m ? `${m} ${key}` : key;
    },

    async loadMcp() {
      this.mcp.loading = true;
      try {
        const [trusted, audit] = await Promise.all([
          this.api('GET', '/api/mcp/trusted'),
          this.api('GET', '/api/mcp/audit?limit=80'),
        ]);
        this.mcp.trusted = trusted;
        // audit endpoint returns { recent: [...], total: N } per
        // audit_log_snapshot_json's shape. Tolerate both arr-only and
        // wrapped shapes.
        this.mcp.audit = Array.isArray(audit) ? audit
          : Array.isArray(audit.recent) ? audit.recent
          : Array.isArray(audit.entries) ? audit.entries
          : [];
        this.mcp.loaded = true;
      } catch (e) {
        this.toast(this.t('web.mcp.toast.load_failed').replace('{err}', e.message), 'error');
      } finally {
        this.mcp.loading = false;
      }
    },

    async trustAgent(name) {
      const nm = (name || this.mcp.newAgentInput).trim();
      if (!nm) {
        this.toast(this.t('web.mcp.toast.name_required'), 'error');
        return;
      }
      try {
        await this.api('POST', '/api/mcp/trust', { name: nm });
        this.mcp.newAgentInput = '';
        await this.loadMcp();
        this.toast(this.t('web.mcp.toast.trusted').replace('{name}', nm));
      } catch (e) {
        this.toast(this.t('web.mcp.toast.trust_failed').replace('{err}', e.message), 'error');
      }
    },

    async untrustAgent(name) {
      if (!confirm(this.t('web.mcp.confirm.untrust').replace('{name}', name))) return;
      try {
        await this.api('POST', '/api/mcp/untrust', { name });
        await this.loadMcp();
        this.toast(this.t('web.mcp.toast.untrusted').replace('{name}', name));
      } catch (e) {
        this.toast(this.t('web.mcp.toast.trust_failed').replace('{err}', e.message), 'error');
      }
    },

    // ---- Identity profiles ----
    //
    // All mutations re-fetch the full list afterward rather than
    // optimistically patching local state. Profile data is small (one
    // TOML file per profile) and the registry-load on the server side
    // is fast, so the safety of "what's displayed always matches what's
    // on disk" wins over micro-optimization.

    async loadProfiles() {
      this.profiles.loading = true;
      try {
        const [data, ssh] = await Promise.all([
          this.api('GET', '/api/profile/list'),
          this.api('GET', '/api/profile/ssh-include-status'),
        ]);
        this.profiles.list = data.profiles || [];
        this.profiles.defaultId = data.default;
        this.profiles.sshInclude = ssh;
        // Seed inline forms for each profile so x-model bindings have
        // something to write into. Missing entries → empty defaults.
        for (const p of this.profiles.list) {
          if (!this.profiles.secretForms[p.id]) this.profiles.secretForms[p.id] = { env_name: '', value: '' };
          if (!this.profiles.gitForms[p.id]) this.profiles.gitForms[p.id] = { user_name: p.git.user_name, user_email: p.git.user_email };
          if (!this.profiles.sshForms[p.id]) this.profiles.sshForms[p.id] = { host: '', key_path: '' };
        }
        this.profiles.loaded = true;
      } catch (e) {
        this.toast(this.t('web.toast.profile_load_failed').replace('{err}', e.message), 'error');
      } finally {
        this.profiles.loading = false;
      }
    },

    async createProfile() {
      const dn = this.profiles.newForm.display_name.trim();
      if (!dn) {
        this.toast(this.t('web.profiles.toast.name_required'), 'error');
        return;
      }
      try {
        await this.api('POST', '/api/profile/create', {
          display_name: dn,
          accent_color: this.profiles.newForm.accent_color || undefined,
        });
        this.profiles.newForm = { open: false, display_name: '', accent_color: '#10b981' };
        await this.loadProfiles();
        this.toast(this.t('web.profiles.toast.created').replace('{name}', dn));
      } catch (e) {
        this.toast(this.t('web.profiles.toast.create_failed').replace('{err}', e.message), 'error');
      }
    },

    async deleteProfile(id, displayName) {
      if (!confirm(this.t('web.profiles.confirm.delete').replace('{name}', displayName))) return;
      try {
        await this.api('DELETE', '/api/profile/' + encodeURIComponent(id));
        await this.loadProfiles();
        this.toast(this.t('web.profiles.toast.deleted').replace('{name}', displayName));
      } catch (e) {
        this.toast(this.t('web.profiles.toast.delete_failed').replace('{err}', e.message), 'error');
      }
    },

    async setProfileAsDefault(id) {
      try {
        await this.api('POST', '/api/profile/set-default', { id });
        await this.loadProfiles();
        this.toast(this.t('web.profiles.toast.default_set'));
      } catch (e) {
        this.toast(this.t('web.profiles.toast.default_failed').replace('{err}', e.message), 'error');
      }
    },

    async addSecret(id) {
      const f = this.profiles.secretForms[id];
      if (!f || !f.env_name.trim() || !f.value) {
        this.toast(this.t('web.profiles.toast.secret_required'), 'error');
        return;
      }
      try {
        await this.api('POST', '/api/profile/' + encodeURIComponent(id) + '/secret', {
          env_name: f.env_name.trim(),
          value: f.value,
        });
        this.profiles.secretForms[id] = { env_name: '', value: '' };
        await this.loadProfiles();
        this.toast(this.t('web.profiles.toast.secret_saved'));
      } catch (e) {
        this.toast(this.t('web.profiles.toast.secret_failed').replace('{err}', e.message), 'error');
      }
    },

    async deleteSecret(id, envName) {
      if (!confirm(this.t('web.profiles.confirm.secret_delete').replace('{env}', envName))) return;
      try {
        await this.api(
          'DELETE',
          '/api/profile/' + encodeURIComponent(id) + '/secret/' + encodeURIComponent(envName),
        );
        await this.loadProfiles();
      } catch (e) {
        this.toast(this.t('web.profiles.toast.secret_failed').replace('{err}', e.message), 'error');
      }
    },

    async saveGitIdentity(id) {
      const g = this.profiles.gitForms[id] || {};
      try {
        await this.api('PUT', '/api/profile/' + encodeURIComponent(id), {
          git: { user_name: g.user_name || '', user_email: g.user_email || '' },
        });
        await this.loadProfiles();
        this.toast(this.t('web.profiles.toast.git_saved'));
      } catch (e) {
        this.toast(this.t('web.profiles.toast.git_failed').replace('{err}', e.message), 'error');
      }
    },

    async addSshRoute(id, profile) {
      const f = this.profiles.sshForms[id];
      if (!f || !f.host.trim() || !f.key_path.trim()) {
        this.toast(this.t('web.profiles.toast.ssh_required'), 'error');
        return;
      }
      const merged = Object.assign({}, profile.ssh, { [f.host.trim()]: f.key_path.trim() });
      try {
        await this.api('PUT', '/api/profile/' + encodeURIComponent(id), { ssh: merged });
        this.profiles.sshForms[id] = { host: '', key_path: '' };
        await this.loadProfiles();
        this.toast(this.t('web.profiles.toast.ssh_saved'));
      } catch (e) {
        this.toast(this.t('web.profiles.toast.ssh_failed').replace('{err}', e.message), 'error');
      }
    },

    async removeSshRoute(id, profile, host) {
      const next = Object.assign({}, profile.ssh);
      delete next[host];
      try {
        await this.api('PUT', '/api/profile/' + encodeURIComponent(id), { ssh: next });
        await this.loadProfiles();
      } catch (e) {
        this.toast(this.t('web.profiles.toast.ssh_failed').replace('{err}', e.message), 'error');
      }
    },

    async installSshInclude() {
      try {
        const r = await this.api('POST', '/api/profile/install-ssh-include');
        await this.loadProfiles();
        this.toast(
          r.already_present
            ? this.t('web.profiles.toast.ssh_include_already')
            : this.t('web.profiles.toast.ssh_include_installed'),
        );
      } catch (e) {
        this.toast(this.t('web.profiles.toast.ssh_include_failed').replace('{err}', e.message), 'error');
      }
    },

    // ---- Onboarding wizard ----

    async runImportWizard() {
      this.wizard.active = true;
      this.wizard.loading = true;
      this.wizard.candidates = [];
      this.wizard.selected = {};
      this.wizard.manualValues = {};
      try {
        const r = await this.api('GET', '/api/profile/import-scan');
        this.wizard.candidates = r.candidates || [];
        // Default-select candidates whose values we can extract; leave
        // manual-paste candidates unchecked so the user has to opt in.
        for (let i = 0; i < this.wizard.candidates.length; i++) {
          this.wizard.selected[i] = this.wizard.candidates[i].has_value;
        }
      } catch (e) {
        this.toast(this.t('web.profiles.toast.scan_failed').replace('{err}', e.message), 'error');
      } finally {
        this.wizard.loading = false;
      }
    },

    cancelWizard() {
      this.wizard.active = false;
    },

    async finalizeWizard() {
      const profileName = this.wizard.profileName.trim() || 'Personal';
      const picks = this.wizard.candidates
        .map((c, i) => ({ c, i }))
        .filter(({ i }) => this.wizard.selected[i]);

      if (picks.length === 0) {
        this.toast(this.t('web.profiles.wizard.toast.nothing_selected'), 'error');
        return;
      }

      // Validate: every selected has-no-value candidate needs a manual
      // value the user pasted in. Surface ALL missing in one go so they
      // can fill them and resubmit, rather than fighting one at a time.
      const missing = picks.filter(({ c, i }) => !c.has_value && !this.wizard.manualValues[i]);
      if (missing.length > 0) {
        this.toast(
          this.t('web.profiles.wizard.toast.missing_value').replace('{count}', missing.length),
          'error',
        );
        return;
      }

      try {
        // Step 1: create the profile.
        const created = await this.api('POST', '/api/profile/create', {
          display_name: profileName,
          accent_color: this.wizard.accentColor,
        });
        const id = created.id;

        // Step 2: for each picked candidate that has a value, we have
        // to fetch it from the server now (the scan API doesn't return
        // raw values for safety). For has_value=false, use the manual
        // paste. For has_value=true, run a single-source rescan and
        // match by env_name to pull the value out.
        // Simplification: in the SPA, has_value=true candidates send
        // env_name to the server which re-runs the matching sniffer
        // and stores the value directly (server already has access).
        // For now, since /api/profile/import-scan doesn't expose
        // values, has_value=true picks need a follow-up "import-fetch"
        // call. Wire that in when the time comes. v1 simpler path:
        // ONLY accept manual paste, so we don't need the value-fetch
        // endpoint. has_value=true serves as a discovery hint only.
        for (const { c, i } of picks) {
          if (!this.wizard.manualValues[i]) continue; // need a value to store
          await this.api('POST', '/api/profile/' + encodeURIComponent(id) + '/secret', {
            env_name: c.suggested_env_name,
            value: this.wizard.manualValues[i],
          });
        }

        // Step 3: for SSH-type candidates, also write the host→key
        // routing into the profile's [ssh] table.
        const sshPicks = picks.filter(({ c }) => c.source === 'ssh' && c.host);
        if (sshPicks.length > 0) {
          const sshMap = {};
          for (const { c } of sshPicks) {
            // c.label includes the key path after the arrow; pull it.
            const m = c.label.match(/→\s+(.+)$/);
            if (m && c.host) sshMap[c.host] = m[1];
          }
          if (Object.keys(sshMap).length > 0) {
            await this.api('PUT', '/api/profile/' + encodeURIComponent(id), { ssh: sshMap });
          }
        }

        this.wizard.active = false;
        await this.loadProfiles();
        this.toast(
          this.t('web.profiles.wizard.toast.success').replace('{name}', profileName),
        );
      } catch (e) {
        this.toast(this.t('web.profiles.wizard.toast.failed').replace('{err}', e.message), 'error');
      }
    },

    async applyTheme(id) {
      try {
        await this.api('POST', '/api/theme', { name: id });
        this.state.theme = id;
        this.toast(this.t('web.toast.theme_applied').replace('{id}', id));
      } catch (e) {
        this.toast(this.t('web.toast.theme_failed').replace('{err}', e.message), 'error');
      }
    },

    async toggleProxy(enabled) {
      try {
        await this.api('POST', '/api/proxy', { enabled });
        await this.refresh();
        this.toast(enabled ? this.t('web.toast.proxy_enabled') : this.t('web.toast.proxy_disabled'));
      } catch (e) {
        this.toast(this.t('web.toast.proxy_failed').replace('{err}', e.message), 'error');
      }
    },

    async saveProxyManual() {
      try {
        await this.api('POST', '/api/proxy', {
          enabled: true,
          http_proxy: this.proxyForm.http_proxy || undefined,
          socks_proxy: this.proxyForm.socks_proxy || undefined,
          no_proxy: this.proxyForm.no_proxy || undefined,
        });
        await this.refresh();
        this.toast(this.t('web.toast.proxy_saved'));
      } catch (e) {
        this.toast(this.t('web.toast.proxy_failed').replace('{err}', e.message), 'error');
      }
    },

    async toggleRotation(enabled) {
      try {
        const r = await this.api('POST', '/api/proxy/rotation', { enabled });
        if (!this.proxy.rotation) this.proxy.rotation = {};
        this.proxy.rotation = r;
        // Enabling rotation also turns the proxy on server-side; refresh so the
        // main proxy toggle + status reflect it (and the status bar reads on).
        if (enabled) await this.refresh();
        this.toast(enabled ? this.t('web.toast.rotation_on') : this.t('web.toast.rotation_off'));
      } catch (e) {
        this.toast(this.t('web.toast.proxy_failed').replace('{err}', e.message), 'error');
      }
    },

    async toggleNodeInPool(name, checked) {
      const pool = ((this.proxy.rotation && this.proxy.rotation.pool) || []).slice();
      const idx = pool.indexOf(name);
      if (checked && idx === -1) pool.push(name);
      else if (!checked && idx !== -1) pool.splice(idx, 1);
      // In clash mode, the pool is node names *within* the selected group, so
      // we pin the group alongside the pool.
      const body = this.clash.connected ? { pool, group: this.clashGroup } : { pool };
      try {
        const r = await this.api('POST', '/api/proxy/rotation', body);
        this.proxy.rotation = r;
      } catch (e) {
        this.toast(this.t('web.toast.proxy_failed').replace('{err}', e.message), 'error');
      }
    },

    // ---- Clash/mihomo: read groups + nodes, you tick boxes ----
    async loadClash() {
      try {
        const c = await this.api('GET', '/api/proxy/clash');
        this.clash = { connected: !!c.connected, version: c.version || '', controller: c.controller || '', groups: c.groups || [] };
        if (this.clash.connected && this.clash.groups.length) {
          // Default the dropdown to the saved rotation group; otherwise pick a
          // sensible "manual select" group: prefer one whose name reads like a
          // node picker, skip the GLOBAL meta-group, and fall back to the
          // largest remaining group.
          const saved = this.proxy.rotation && this.proxy.rotation.group;
          if (saved && this.clash.groups.some((g) => g.name === saved)) {
            this.clashGroup = saved;
          } else if (!this.clashGroup || !this.clash.groups.some((g) => g.name === this.clashGroup)) {
            this.clashGroup = this.pickDefaultGroup();
          }
        }
      } catch (e) {
        this.clash = { connected: false, version: '', controller: '', groups: [] };
      }
    },

    pickDefaultGroup() {
      const groups = this.clash.groups || [];
      if (!groups.length) return '';
      // Prefer a group that reads like a manual node picker.
      const prefer = /选择|节点|proxy|select|🚀|手动/i;
      const named = groups.filter((g) => prefer.test(g.name));
      const pool = (named.length ? named : groups.filter((g) => g.name !== 'GLOBAL'));
      const candidates = pool.length ? pool : groups;
      return candidates.slice().sort((a, b) => b.nodes.length - a.nodes.length)[0].name;
    },

    async setClashController() {
      try {
        const c = await this.api('POST', '/api/proxy/clash/controller', {
          controller: (this.clashCtl.controller || '').trim(),
          secret: (this.clashCtl.secret || '').trim(),
        });
        this.clash = { connected: !!c.connected, version: c.version || '', controller: c.controller || '', groups: c.groups || [] };
        if (this.clash.connected) {
          if (this.clash.groups.length) this.clashGroup = this.pickDefaultGroup();
          this.toast(this.t('web.toast.clash_connected'));
        } else {
          this.toast(this.t('web.toast.clash_failed'), 'error');
        }
      } catch (e) {
        this.toast(this.t('web.toast.proxy_failed').replace('{err}', e.message), 'error');
      }
    },

    clashGroupObj() {
      return (this.clash.groups || []).find((g) => g.name === this.clashGroup) || null;
    },

    clashNow() {
      const g = this.clashGroupObj();
      return g ? g.now : '';
    },

    filteredNodes() {
      const g = this.clashGroupObj();
      if (!g) return [];
      const f = (this.nodeFilter || '').trim().toLowerCase();
      const nodes = f ? g.nodes.filter((n) => n.name.toLowerCase().includes(f)) : g.nodes;
      // Alive + lowest latency first; unknown/dead sink to the bottom.
      return nodes.slice().sort((a, b) => {
        const da = a.delay || (a.alive ? 99998 : 99999);
        const db = b.delay || (b.alive ? 99998 : 99999);
        return da - db;
      });
    },

    async onGroupChange() {
      // Switching the group resets the pool to that group's checked nodes —
      // persist the new group so rotation operates on it.
      try {
        const r = await this.api('POST', '/api/proxy/rotation', { group: this.clashGroup, pool: [] });
        this.proxy.rotation = r;
      } catch (e) {
        this.toast(this.t('web.toast.proxy_failed').replace('{err}', e.message), 'error');
      }
    },

    async setRotationInterval(val) {
      const interval_secs = Math.max(5, parseInt(val, 10) || 30);
      try {
        const r = await this.api('POST', '/api/proxy/rotation', { interval_secs });
        this.proxy.rotation = r;
      } catch (e) {
        this.toast(this.t('web.toast.proxy_failed').replace('{err}', e.message), 'error');
      }
    },

    // Persist the node list (name+url pairs) so the rotation pool can be built
    // from the GUI. The server returns the freshly probed list + pruned pool.
    async saveNodes(nodes) {
      const r = await this.api('POST', '/api/proxy/nodes', { nodes });
      this.proxy.nodes = r.nodes || [];
      if (this.proxy.rotation) this.proxy.rotation.pool = r.pool || [];
    },

    async addNode() {
      const name = (this.newNode.name || '').trim();
      const url = (this.newNode.url || '').trim();
      if (!name || !url) {
        this.toast(this.t('web.toast.node_need_both'), 'error');
        return;
      }
      const nodes = [...(this.proxy.nodes || []).map((n) => ({ name: n.name, url: n.url })), { name, url }];
      try {
        await this.saveNodes(nodes);
        this.newNode = { name: '', url: '' };
        this.toast(this.t('web.toast.node_added'));
      } catch (e) {
        this.toast(this.t('web.toast.proxy_failed').replace('{err}', e.message), 'error');
      }
    },

    async addCurrentProxyNode() {
      const url = this.proxy.http_proxy || this.proxy.socks_proxy;
      if (!url) return;
      // Derive a name from the port, keeping it unique.
      const port = (url.match(/:(\d+)/) || [])[1] || 'proxy';
      let name = 'proxy-' + port;
      const taken = new Set((this.proxy.nodes || []).map((n) => n.name));
      let i = 2;
      while (taken.has(name)) name = 'proxy-' + port + '-' + i++;
      const nodes = [...(this.proxy.nodes || []).map((n) => ({ name: n.name, url: n.url })), { name, url }];
      try {
        await this.saveNodes(nodes);
        this.toast(this.t('web.toast.node_added'));
      } catch (e) {
        this.toast(this.t('web.toast.proxy_failed').replace('{err}', e.message), 'error');
      }
    },

    async removeNode(name) {
      const nodes = (this.proxy.nodes || [])
        .filter((n) => n.name !== name)
        .map((n) => ({ name: n.name, url: n.url }));
      try {
        await this.saveNodes(nodes);
        this.toast(this.t('web.toast.node_removed'));
      } catch (e) {
        this.toast(this.t('web.toast.proxy_failed').replace('{err}', e.message), 'error');
      }
    },

    async openSession(s) {
      try {
        const md = await this.api(
          'GET',
          '/api/sessions/' + encodeURIComponent(s.unterm_session_id) + '/markdown'
        );
        this.sessionMarkdown = md;
        this.currentSessionId = s.unterm_session_id;
      } catch (e) {
        this.toast(this.t('web.toast.session_failed').replace('{err}', e.message), 'error');
      }
    },

    toast(text, kind = 'ok') {
      const id = this.nextToast++;
      this.toasts.push({ id, text, kind });
      setTimeout(() => {
        this.toasts = this.toasts.filter((t) => t.id !== id);
      }, 3500);
    },
  };
}
