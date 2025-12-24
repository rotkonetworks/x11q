import init, {
    init as initServer,
    reset_connection,
    process_x11_data,
    handle_keydown,
    handle_keyup,
    handle_mousemove,
    handle_mousebutton,
    render,
    get_pending_events
} from './pkg/x11q_web.js';

class X11Client {
    constructor() {
        this.ws = null;
        this.canvas = document.getElementById('canvas');
        this.status = document.getElementById('status');
        this.connected = false;
        this.x11Connected = false; // True after X11 connection setup complete
    }

    async start() {
        await init();
        await initServer('canvas');
        this.setupInput();
        this.startRenderLoop();
        this.setStatus('ready', 'connecting');

        // Auto-connect to websocket
        const wsUrl = `ws://${window.location.host}/ws`;
        this.connect(wsUrl);
    }

    connect(url) {
        this.setStatus('connecting...', 'connecting');

        this.ws = new WebSocket(url);
        this.ws.binaryType = 'arraybuffer';

        this.ws.onopen = () => {
            this.connected = true;
            this.x11Connected = false;
            // Reset server state for new connections
            reset_connection();
            this.setStatus('waiting for x11 client', 'connecting');
        };

        this.ws.onmessage = (event) => {
            const data = new Uint8Array(event.data);
            console.log('x11 request:', data.length, 'bytes, first byte:', data[0]);
            try {
                const response = process_x11_data(data);
                if (response && response.length > 0) {
                    console.log('x11 response:', response.length, 'bytes');
                    this.ws.send(response);

                    // After first successful response, we're fully connected
                    if (!this.x11Connected) {
                        this.x11Connected = true;
                        this.setStatus('connected', 'connected');
                        this.startEventLoop();
                    }
                } else {
                    console.log('no response needed');
                }
            } catch (e) {
                console.error('process error:', e);
                // Don't let errors crash the connection
            }
        };

        this.ws.onclose = () => {
            this.connected = false;
            this.x11Connected = false;
            this.setStatus('disconnected', 'disconnected');
            // Reconnect after delay
            setTimeout(() => this.connect(url), 2000);
        };

        this.ws.onerror = (e) => {
            console.error('ws error:', e);
        };
    }

    startEventLoop() {
        const sendEvents = () => {
            if (!this.connected || !this.x11Connected || this.ws.readyState !== WebSocket.OPEN) {
                return;
            }

            try {
                const events = get_pending_events();
                if (events && events.length > 0) {
                    this.ws.send(events);
                }
            } catch (e) {
                console.error('event error:', e);
            }

            setTimeout(sendEvents, 16);
        };
        sendEvents();
    }

    startRenderLoop() {
        const frame = () => {
            try {
                render();
            } catch (e) {
                // WebGPU may not be ready yet
            }
            requestAnimationFrame(frame);
        };
        requestAnimationFrame(frame);
    }

    setupInput() {
        const modifiers = (e) => {
            let m = 0;
            if (e.shiftKey) m |= 1;
            if (e.ctrlKey) m |= 4;
            if (e.altKey) m |= 8;
            if (e.metaKey) m |= 64;
            return m;
        };

        this.canvas.tabIndex = 0;
        this.canvas.focus();

        this.canvas.addEventListener('keydown', (e) => {
            if (!this.x11Connected) return;
            e.preventDefault();
            handle_keydown(e.code, e.key, modifiers(e));
        });

        this.canvas.addEventListener('keyup', (e) => {
            if (!this.x11Connected) return;
            e.preventDefault();
            handle_keyup(e.code, e.key, modifiers(e));
        });

        this.canvas.addEventListener('mousemove', (e) => {
            if (!this.x11Connected) return;
            const rect = this.canvas.getBoundingClientRect();
            const x = Math.floor(e.clientX - rect.left);
            const y = Math.floor(e.clientY - rect.top);
            handle_mousemove(x, y);
        });

        this.canvas.addEventListener('mousedown', (e) => {
            if (!this.x11Connected) return;
            e.preventDefault();
            this.canvas.focus();
            const rect = this.canvas.getBoundingClientRect();
            const x = Math.floor(e.clientX - rect.left);
            const y = Math.floor(e.clientY - rect.top);
            handle_mousebutton(e.button + 1, true, x, y);
        });

        this.canvas.addEventListener('mouseup', (e) => {
            if (!this.x11Connected) return;
            e.preventDefault();
            const rect = this.canvas.getBoundingClientRect();
            const x = Math.floor(e.clientX - rect.left);
            const y = Math.floor(e.clientY - rect.top);
            handle_mousebutton(e.button + 1, false, x, y);
        });

        this.canvas.addEventListener('contextmenu', (e) => e.preventDefault());

        this.canvas.addEventListener('wheel', (e) => {
            if (!this.x11Connected) return;
            e.preventDefault();
            const rect = this.canvas.getBoundingClientRect();
            const x = Math.floor(e.clientX - rect.left);
            const y = Math.floor(e.clientY - rect.top);
            const button = e.deltaY < 0 ? 4 : 5;
            handle_mousebutton(button, true, x, y);
            handle_mousebutton(button, false, x, y);
        });
    }

    setStatus(text, cls) {
        this.status.textContent = text;
        this.status.className = cls;
    }
}

const client = new X11Client();
window.addEventListener('load', () => client.start());
