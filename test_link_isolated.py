import aalink
import asyncio
import time

async def main():
    loop = asyncio.get_running_loop()
    # Try different quantum / beat values?
    link = aalink.Link(120.0, loop)
    link.enabled = True
    # link.start_stop_sync_enabled = True # maybe?
    
    print("Link Enabled. Waiting for peers...")
    
    while True:
        # Access properties
        bpm = link.tempo
        peers = link.num_peers
        beat = link.beat
        
        print(f"Peers: {peers} | BPM: {bpm} | Beat: {beat:.2f}", end='\r')
        await asyncio.sleep(0.1)

if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        pass
