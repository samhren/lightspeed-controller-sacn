import rumps
import threading
import socket
import webbrowser
import logging
import psutil
from flask import Flask, render_template, request, jsonify
from lighting_engine import LightingEngine
from link_manager import LinkManager

# --- Configuration ---
HOST = '127.0.0.1'
PORT = 5001

# --- Flask App Setup ---
app = Flask(__name__)
# Disable Flask logging clutter
log = logging.getLogger('werkzeug')
log.setLevel(logging.ERROR)

engine = LightingEngine(fps=30)
link_mgr = LinkManager()

@app.route('/')
def index():
    return render_template('index.html')

@app.route('/api/bpm', methods=['GET'])
def get_bpm():
    return jsonify({
        "bpm": link_mgr.bpm,
        "peers": link_mgr.num_peers
    })

@app.route('/strips', methods=['GET'])
def get_strips():
    # Return list of strips
    return jsonify([s.to_dict() for s in engine.strips])

@app.route('/add_strip', methods=['POST'])
def add_strip():
    data = request.json
    universe = int(data.get('universe'))
    count = int(data.get('count'))
    # add_strip now returns the ID directly
    strip_id = engine.add_strip(universe, count)
    return jsonify({"status": "success", "id": strip_id})

@app.route('/delete_strip', methods=['POST'])
def delete_strip():
    data = request.json
    strip_id = data.get('id')
    engine.delete_strip(strip_id)
    return jsonify({"status": "deleted", "id": strip_id})

@app.route('/update_strip', methods=['POST'])
def update_strip():
    data = request.json
    engine.update_strip(
        id=data.get('id'),
        universe=data.get('universe'),
        count=data.get('count'),
        x=data.get('x'),
        y=data.get('y'),
        spacing=data.get('spacing'),
        rotation=data.get('rotation')
    )
    return jsonify({"status": "updated"})

@app.route('/update_layout', methods=['POST'])
def update_layout():
    updates = request.json # Expect list of {id, x, y}
    for item in updates:
        engine.update_strip_position(item['id'], item['x'], item['y'])
    return jsonify({"status": "updated"})

@app.route('/set_effect', methods=['POST'])
def set_effect():
    data = request.json
    effect = data.get('effect')
    engine.set_effect(effect)
    return jsonify({"status": "effect_set", "effect": effect})

@app.route('/set_mode', methods=['POST'])
def set_mode():
    data = request.json
    mode = data.get('mode')
    engine.set_mode(mode)
    return jsonify({"status": "mode_set", "mode": mode})

# --- Mask API ---
@app.route('/masks', methods=['GET'])
def get_masks():
    return jsonify([m.to_dict() for m in engine.masks])

@app.route('/add_mask', methods=['POST'])
def add_mask():
    data = request.json
    m_type = data.get('type')
    # Default center
    m_id = engine.add_mask(m_type, params=data.get('params'))
    return jsonify({"status": "success", "id": m_id})

@app.route('/update_mask', methods=['POST'])
def update_mask():
    data = request.json
    m_id = data.get('id')
    engine.update_mask(m_id, x=data.get('x'), y=data.get('y'), params=data.get('params'))
    return jsonify({"status": "updated"})

@app.route('/delete_mask', methods=['POST'])
def delete_mask():
    data = request.json
    m_id = data.get('id')
    engine.delete_mask(m_id)
    return jsonify({"status": "deleted"})

@app.route('/interfaces', methods=['GET'])
def get_interfaces():
    interfaces = []
    # Get all network interfaces
    for name, addrs in psutil.net_if_addrs().items():
        for addr in addrs:
            if addr.family == socket.AF_INET:  # IPv4 only
                interfaces.append({"name": name, "ip": addr.address})
    return jsonify(interfaces)

@app.route('/set_network', methods=['POST'])
def set_network():
    data = request.json
    ip = data.get('ip')
    success = engine.start_sender(bind_address=ip)
    if success:
        return jsonify({"status": "success", "ip": ip})
    else:
        return jsonify({"status": "error", "message": "Failed to bind to IP"}), 500

@app.route('/network_status', methods=['GET'])
def get_network_status():
    return jsonify({
        "current_ip": engine.bind_address,
        "mode": engine.mode,
        "effect": engine.current_effect
    })

# --- Rumps App Class ---
class PixelControllerApp(rumps.App):
    def __init__(self):
        super(PixelControllerApp, self).__init__("PixelControl", icon=None)
        self.menu = ["Open Web Interface", "Quit"]
        self.quit_button = None # We handle quit manually via menu item matching

    @rumps.clicked("Open Web Interface")
    def open_web(self, _):
        url = f"http://{HOST}:{PORT}"
        webbrowser.open(url)

    @rumps.clicked("Quit")
    def quit_app(self, _):
        print("Quitting app...")
        engine.stop()
        rumps.quit_application()

def start_flask():
    try:
        app.run(host=HOST, port=PORT, debug=False, use_reloader=False)
    except Exception as e:
        print(f"Flask Error: {e}")

# --- Main Entry Point ---
if __name__ == '__main__':
    print("Starting Lighting Engine...")
    # Try to start on default; if it fails, user can set via UI
    engine.load_state() 
    if not engine.sender: # If load_state didn't start a sender (no saved ip)
        engine.start_sender() 
    engine.start()
    link_mgr.start() # Start link_mgr thread

    print("Starting Web Server...")
    server_thread = threading.Thread(target=start_flask)
    server_thread.daemon = True
    server_thread.start()

    print("Starting Menu Bar App...")
    PixelControllerApp().run()
