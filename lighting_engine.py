import time
import threading
import math
import sacn
import json
import os
from dataclasses import dataclass, field, asdict
from typing import List, Dict, Tuple, Any
import numpy as np
import shapely
from shapely.geometry import Point, Polygon, box
from shapely.prepared import prep
from shapely import affinity

CONFIG_FILE = "lighting_config.json"

@dataclass
class Mask:
    id: int
    type: str # "scanner", "radial"
    x: float = 0.5
    y: float = 0.5
    # Generic params: width/radius, speed related (though interactive masks might just have pos), color
    params: Dict = field(default_factory=dict)
    
    def to_dict(self):
        return {
            "id": self.id,
            "type": self.type,
            "x": self.x,
            "y": self.y,
            "params": self.params
        }

@dataclass
class PixelStrip:
    universe: int
    start_channel: int
    pixel_count: int
    x: float = 0.5
    y: float = 0.5
    id: int = 0
    # Physical properties
    spacing: float = 0.01
    rotation: float = 0.0
    
    # Runtime buffer
    data: List[Tuple[int, int, int]] = field(init=False)

    def __post_init__(self):
        self.data = [(0, 0, 0)] * self.pixel_count
        if not hasattr(self, 'id'): 
             self.id = 0

    def to_dict(self):
        return {
            "id": self.id,
            "universe": self.universe,
            "pixel_count": self.pixel_count,
            "start_channel": self.start_channel,
            "x": self.x,
            "y": self.y,
            "spacing": self.spacing,
            "rotation": self.rotation
        }

class LightingEngine(threading.Thread):
    def __init__(self, fps=30):
        super().__init__()
        self.fps = fps
        self.running = False
        self.strips: List[PixelStrip] = []
        self.masks: List[Mask] = [] # Interactive masks
        self.sender = None
        self.bind_address = None
        self.active_universes = set()
        
        self.mode = "spatial" # "global" or "spatial"
        self.current_effect = "scanner" # Global effect name
        self.lock = threading.Lock()
        self.start_time = time.time()
        
        # Effect parameters (Global)
        self.speed = 1.0
        self.hue_offset = 0
        
        # Geometry Cache: {strip_id: (np_points_xy, shapely_points_array)}
        # We store both purely numeric coordinates (for gradients) and shapely objects (for fast containment)
        self.pixel_caches: Dict[int, Tuple[np.ndarray, Any]] = {}

    def start_sender(self, bind_address=None):
        with self.lock:
            # Stop existing sender if running
            if self.sender:
                try:
                    self.sender.stop()
                except Exception:
                    pass
                self.sender = None
            
            self.bind_address = bind_address
            self.active_universes = set() # Reset for new sender
            
            try:
                # Use default if None provided
                addr = bind_address if bind_address else "0.0.0.0"
                print(f"Attempting to start sACN sender on {addr}...")
                
                new_sender = sacn.sACNsender(bind_address=addr)
                new_sender.start()
                
                self.sender = new_sender
                
                # Re-activate outputs for existing strips
                for strip in self.strips:
                     self._ensure_universes_active(strip)
                
                print(f"sACN Sender started successfully on {addr}")
                self.save_state()
                return True
            except Exception as e:
                print(f"ERROR: Failed to start sACN sender on {bind_address if bind_address else 'default'}: {e}")
                self.sender = None 
                return False

    def set_mode(self, mode):
        with self.lock:
            self.mode = mode
            print(f"Mode switched to: {mode}")
            # Reset effect to sensible default for mode?
            if mode == "global" and self.current_effect == "scanner":
               self.current_effect = "rainbow"
            elif mode == "spatial" and self.current_effect == "rainbow":
               self.current_effect = "scanner"
            self.save_state()

    def set_effect(self, effect_name):
        with self.lock:
            self.current_effect = effect_name
            print(f"Effect switched to: {effect_name}")
            self.save_state()

    def save_state(self):
        # Save strips to JSON
        data = {
            "strips": [s.to_dict() for s in self.strips],
            "masks": [m.to_dict() for m in self.masks], # Save masks
            "bind_address": self.bind_address,
            "mode": self.mode,
            "effect": self.current_effect
        }
        try:
            with open(CONFIG_FILE, 'w') as f:
                json.dump(data, f, indent=4)
        except Exception as e:
            print(f"Failed to save state: {e}")

    def load_state(self):
        if not os.path.exists(CONFIG_FILE):
            return
        
        try:
            with open(CONFIG_FILE, 'r') as f:
                data = json.load(f)
            
            # Restore settings
            self.bind_address = data.get("bind_address")
            if self.bind_address:
                 self.start_sender(self.bind_address)
                 
            self.mode = data.get("mode", "spatial")
            self.current_effect = data.get("effect", "scanner")

            with self.lock:
                # Restore strips
                self.strips = [] 
                self.pixel_caches = {} # Clear cache
                for s_data in data.get("strips", []):
                    strip = PixelStrip(
                        s_data['universe'], 
                        s_data['pixel_count'], 
                        s_data['start_channel'],
                        x=s_data.get('x'),
                        y=s_data.get('y'),
                        id=s_data.get('id'),
                        spacing=s_data.get('spacing', 0.01),
                        rotation=s_data.get('rotation', 0.0)
                    )
                    self.strips.append(strip)
                    self._ensure_universes_active(strip)
                    self._recalculate_geometry(strip) # Build cache
                
                # Restore masks
                self.masks = []
                for m_data in data.get("masks", []):
                    self.masks.append(Mask(
                        m_data['id'],
                        m_data['type'],
                        m_data['x'],
                        m_data['y'],
                        m_data.get('params', {})
                    ))
                    
            print("State loaded successfully")
        except Exception as e:
            print(f"Failed to load state: {e}")
            import traceback
            traceback.print_exc()

    def _recalculate_geometry(self, strip: PixelStrip):
        # 1. Calculate relative positions (unrotated, centered at 0,0)
        # Total length
        total_len = (strip.pixel_count - 1) * strip.spacing
        # Start X relative to center (0,0) before rotation
        # Line is along X axis: from -total_len/2 to +total_len/2
        xs = np.linspace(-total_len/2, total_len/2, strip.pixel_count)
        ys = np.zeros(strip.pixel_count)
        
        # 2. Rotate (Rotation is in radians)
        # x' = x cos θ - y sin θ
        # y' = x sin θ + y cos θ
        cos_t = math.cos(strip.rotation)
        sin_t = math.sin(strip.rotation)
        
        rx = xs * cos_t - ys * sin_t
        ry = xs * sin_t + ys * cos_t
        
        # 3. Translate to Strip Position
        final_x = rx + strip.x
        final_y = ry + strip.y
        
        # Stack into (N, 2) array
        coords = np.column_stack((final_x, final_y))
        
        # Create Shapely points
        shapely_points = shapely.points(final_x, final_y) # Vectorized creation
        
        self.pixel_caches[strip.id] = (coords, shapely_points)

    def add_strip(self, universe, pixel_count, start_channel=1, x=None, y=None, id=None):
        with self.lock:
            # Simple ID generation if not provided
            if id is None:
                id = int(time.time() * 1000)
            
            strip = PixelStrip(universe, start_channel, pixel_count, id=id)
            if x is not None: strip.x = x
            if y is not None: strip.y = y
            
            self.strips.append(strip)
            self._ensure_universes_active(strip)
            self._recalculate_geometry(strip)
            self.save_state()
            return strip.id

    def delete_strip(self, strip_id):
        with self.lock:
            self.strips = [s for s in self.strips if s.id != strip_id]
            if strip_id in self.pixel_caches:
                del self.pixel_caches[strip_id]
            self.save_state()
            print(f"Deleted strip {strip_id}")

    def _ensure_universes_active(self, strip):
        # Helper to activate universes for a strip
        if not self.sender: return
        
        total_channels = strip.pixel_count * 3
        num_universes = (total_channels + 511) // 512
        for i in range(num_universes):
            u = strip.universe + i
            if u not in self.active_universes:
                self.sender.activate_output(u)
                self.sender[u].multicast = True
                self.active_universes.add(u)

    def update_strip_position(self, strip_id, x, y):
        with self.lock:
            for s in self.strips:
                if s.id == strip_id:
                    s.x = x
                    s.y = y
                    self._recalculate_geometry(s)
                    break
            self.save_state()

    def update_strip(self, id, universe=None, count=None, x=None, y=None, spacing=None, rotation=None):
        with self.lock:
            found = False
            for s in self.strips:
                if s.id == id:
                    if universe is not None: s.universe = universe
                    if count is not None: 
                        s.pixel_count = count
                        # Re-init data buffer if count changes
                        s.data = [(0,0,0)] * count
                    if x is not None: s.x = x
                    if y is not None: s.y = y
                    if spacing is not None: s.spacing = spacing
                    if rotation is not None: s.rotation = rotation
                    
                    self._ensure_universes_active(s) # In case universe changed
                    self._recalculate_geometry(s)
                    found = True
                    break
            if found:
                self.save_state()

    # --- Mask Management ---
    def add_mask(self, type, x=0.5, y=0.5, params=None):
        with self.lock:
            id = int(time.time() * 1000)
            mask = Mask(id, type, x, y, params if params else {})
            
            # Defaults
            if type == 'scanner' and 'color' not in mask.params:
                mask.params['color'] = (0, 255, 255)
            if type == 'radial' and 'color' not in mask.params:
                mask.params['color'] = (255, 0, 255)
                
            self.masks.append(mask)
            self.save_state()
            print(f"Added mask {id} ({type})")
            return id

    def update_mask(self, id, x=None, y=None, params=None):
        with self.lock:
            for m in self.masks:
                if m.id == id:
                    if x is not None: m.x = x
                    if y is not None: m.y = y
                    if params: m.params.update(params)
            self.save_state()

    def delete_mask(self, id):
        with self.lock:
            self.masks = [m for m in self.masks if m.id != id]
            self.save_state()
            print(f"Deleted mask {id}")

    # (Original set_effect and save_state duplicated in view, but assuming replace works correctly by line)
    # Be careful with EndLine usage here. I am replacing 1-500.

    # ... Rest of methods ...

    def _apply_spatial_effect(self, t):
        # Clear all strips first
        for strip in self.strips:
            strip.data = [(0,0,0)] * strip.pixel_count

        # Apply each mask additively
        for mask in self.masks:
            self._apply_single_mask(mask, t)

    def _apply_single_mask(self, mask, t):
        # Vectorized Shapely/Numpy Collision
        mx, my = mask.x, mask.y
        m_color = mask.params.get('color', (255, 255, 255))
        r, g, b = m_color
        
        if mask.type == "scanner":
             bound_w = mask.params.get('width', 0.3)
             bound_h = mask.params.get('height', 0.3)
             speed = mask.params.get('speed', 1.0)
             thickness = mask.params.get('thickness', 0.05)
             
             # Create the Bar Polygon
             offset_x = (bound_w / 2) * math.sin(t * speed)
             bar_center_x = mx + offset_x
             
             # Polygon: (center_x, my) with size (thickness, bound_h)
             # box(minx, miny, maxx, maxy)
             minx = bar_center_x - thickness / 2
             maxx = bar_center_x + thickness / 2
             miny = my - bound_h / 2
             maxy = my + bound_h / 2
             
             # The Masking Geometry (The Bar)
             bar_poly = box(minx, miny, maxx, maxy)
             
             # Also create simple Bounds Box for clipping
             # clip_poly = box(mx - bound_w/2, my - bound_h/2, mx + bound_w/2, my + bound_h/2)
             # Optimization: Check clip first? Or just intersect.
             
             for strip in self.strips:
                 if strip.id not in self.pixel_caches: self._recalculate_geometry(strip)
                 coords, shapely_pts = self.pixel_caches[strip.id]
                 
                 # Vectorized Containment Check
                 # Returns boolean array
                 is_inside = shapely.contains(bar_poly, shapely_pts)
                 
                 # If we want gradient, we need distance.
                 # Shapely distance can be slow if done elementwise.
                 # But since bar is axis-aligned (mostly), we can compute dist manually using numpy efficiently.
                 # px, py = coords[:, 0], coords[:, 1]
                 # dist_x = abs(px - bar_center_x)
                 # mask_intensity = 1.0 - (dist_x / thickness)
                 # mask_intensity = clip(0, 1) ...
                 # BUT we only care about those INSIDE Y bounds (handled by shapely contains or manual check)
                 
                 # Let's trust Shapely for "Inside" -> Logic: 1.0 intensity (or simple gradient)
                 # User asked for collisions. "Inside" is a collision.
                 
                 # Let's do a pure Numpy check for the axis-aligned scanner for MAXIMUM SPEED, 
                 # as Shapely `contains` on array is fast but creating Polygons per frame has overhead.
                 # Actually `box` is cheap.
                 
                 # Hybrid:
                 px = coords[:, 0]
                 py = coords[:, 1]
                 
                 # 1. Y Check (Numpy)
                 in_y = np.abs(py - my) <= (bound_h / 2)
                 
                 # 2. X Check (Distance to bar center)
                 dist = np.abs(px - bar_center_x)
                 in_bar = dist <= thickness
                 
                 # 3. Clip Box Check
                 in_clip = np.abs(px - mx) <= (bound_w / 2)
                 
                 # Combined
                 hits = in_y & in_bar & in_clip
                 
                 # Indices where true
                 hit_indices = np.where(hits)[0]
                 
                 if len(hit_indices) > 0:
                     # Calculate intensity: 1.0 - (dist / thickness)
                     # Vectorized
                     dists = dist[hit_indices]
                     intensities = 1.0 - (dists / thickness)
                     
                     # Add to strip data
                     # Need to fetch current data, add, clamp
                     
                     # This is a bit slow to convert back and forth to list of tuples.
                     # Optimization: Keep strip.data as numpy float array (N, 3)?
                     # Refactor: self.strips[i].data IS numpy array?
                     # That would be huge refactor for save/load/sacn sender.
                     # Let's keep data as list for now, but update sparsely.
                     
                     for idx, intensity in zip(hit_indices, intensities):
                         old_r, old_g, old_b = strip.data[idx]
                         # Additive
                         new_r = min(255, int(old_r + r * intensity))
                         new_g = min(255, int(old_g + g * intensity))
                         new_b = min(255, int(old_b + b * intensity))
                         strip.data[idx] = (new_r, new_g, new_b)

        elif mask.type == "radial":
             radius = mask.params.get('radius', 0.2)
             
             for strip in self.strips:
                 if strip.id not in self.pixel_caches: self._recalculate_geometry(strip)
                 coords, _ = self.pixel_caches[strip.id]
                 
                 px = coords[:, 0]
                 py = coords[:, 1]
                 
                 # Vectorized Distance Check
                 dists = np.sqrt((px - mx)**2 + (py - my)**2)
                 
                 hits = dists < radius
                 hit_indices = np.where(hits)[0]
                 
                 if len(hit_indices) > 0:
                     # Simple Gradient 1.0 -> 0.0 at edge
                     intensities = 1.0 - (dists[hit_indices] / radius)
                     
                     for idx, val in zip(hit_indices, intensities):
                         old_r, old_g, old_b = strip.data[idx]
                         new_r = min(255, int(old_r + r * val))
                         new_g = min(255, int(old_g + g * val))
                         new_b = min(255, int(old_b + b * val))
                         strip.data[idx] = (new_r, new_g, new_b)


    def _wheel(self, pos):
        if pos < 85:
            return (pos * 3, 255 - pos * 3, 0)
        elif pos < 170:
            pos -= 85
            return (255 - pos * 3, 0, pos * 3)
        else:
            pos -= 170
            return (0, pos * 3, 255 - pos * 3)
            
    # _transmit_data kept same


    def _transmit_data(self):
        if not self.sender:
            return

        # Frame Buffer approach:
        # 1. Initialize empty buffer for all active universes (Black out everything)
        universe_buffer = {u: [0]*512 for u in self.active_universes}
        
        # 2. Paint strips onto the buffer
        for strip in self.strips:
            # Flat RGB data
            full_data = []
            for r, g, b in strip.data:
                full_data.extend([min(255, max(0, int(r))), 
                                min(255, max(0, int(g))), 
                                min(255, max(0, int(b)))])
            
            total_data_len = len(full_data)
            processed = 0
            
            current_u = strip.universe
            # 1-based start channel -> 0-based index
            current_idx = max(0, strip.start_channel - 1)
            
            while processed < total_data_len:
                # How much space left in this universe?
                space_in_universe = 512 - current_idx
                
                if space_in_universe <= 0:
                    # Should not happen if logic is correct, but safe fallback:
                    current_u += 1
                    current_idx = 0
                    continue

                # How much data works for this chunk?
                chunk_size = min(space_in_universe, total_data_len - processed)
                chunk = full_data[processed : processed + chunk_size]
                
                # Ensure universe exists and is active
                if current_u not in self.active_universes:
                     self.sender.activate_output(current_u)
                     self.sender[current_u].multicast = True
                     self.active_universes.add(current_u)
                     universe_buffer[current_u] = [0]*512
                
                if current_u not in universe_buffer:
                    universe_buffer[current_u] = [0]*512

                # Write data
                for i, val in enumerate(chunk):
                    universe_buffer[current_u][current_idx + i] = val
                
                processed += chunk_size
                current_u += 1
                current_idx = 0 # Subsequent universes start at 0
        
        # 3. Send all buffers
        for u, data in universe_buffer.items():
            self.sender[u].dmx_data = tuple(data)

    # --- Effects ---

    def _clear_all(self):
        for strip in self.strips:
            strip.data = [(0, 0, 0)] * strip.pixel_count

    def _effect_scanner(self, t):
        """A beam that scans back and forth across the X axis."""
        scan_pos = (math.sin(t * self.speed * 2) + 1) / 2  # Oscillate 0.0 - 1.0
        width = 0.15 # Width of the beam
        
        for strip in self.strips:
            # Calculate intensity based on distance from scan_pos
            # In a real grid, x is relevant. 
            dist = abs(strip.x - scan_pos)
            brightness = max(0, 1.0 - (dist / width))
            color = (255 * brightness, 0, 0) # Red beam
            
            # Apply to all pixels in the strip equally for this simple grid effect
            strip.data = [color] * strip.pixel_count

    def _effect_chase(self, t):
        """Pixels chasing each other."""
        offset = int(t * 10 * self.speed)
        
        for strip in self.strips:
            new_data = []
            for i in range(strip.pixel_count):
                if (i + offset) % 5 == 0:
                    new_data.append((0, 255, 0)) # Green dot
                else:
                    new_data.append((0, 0, 0))
            strip.data = new_data

    def _effect_rainbow(self, t):
        """Rainbow cycle."""
        # Function to generate rainbow colors
        def wheel(pos):
            if pos < 85:
                return (pos * 3, 255 - pos * 3, 0)
            elif pos < 170:
                pos -= 85
                return (255 - pos * 3, 0, pos * 3)
            else:
                pos -= 170
                return (0, pos * 3, 255 - pos * 3)

        base_val = int(t * 50) % 255
        
        for strip in self.strips:
            new_data = []
            for i in range(strip.pixel_count):
                pixel_val = (base_val + (i * 256 // strip.pixel_count)) & 255
                new_data.append(wheel(pixel_val))
            strip.data = new_data
