import threading
import time
import asyncio
import aalink

class LinkManager(threading.Thread):
    def __init__(self):
        super().__init__()
        self.running = True
        self.bpm = 120.0
        self.num_peers = 0
        self.lock = threading.Lock()
        self.link = None
        self.loop = None

    def run(self):
        self.loop = asyncio.new_event_loop()
        asyncio.set_event_loop(self.loop)
        self.link = aalink.Link(120.0, self.loop)
        self.link.enabled = True
        
        try:
            self.loop.run_until_complete(self._poll_loop())
        finally:
            self.loop.close()

    async def _poll_loop(self):
        print("LinkManager Started")
        while self.running:
            # Poll Link Session State
            # aalink exposes properties directly
            # Checking if they are methods or properties
            # Based on typical usage, let's try calling them if they follow C++ API, 
            # or access if properties.
            # 'enabled' is a property. 'tempo' might be a method to get current.
            
            with self.lock:
                # Properties, not methods
                self.bpm = self.link.tempo
                self.num_peers = self.link.num_peers
            
            # 20ms poll rate (good enough for visual BPM)
            await asyncio.sleep(0.02)
    
    def stop(self):
        self.running = False
