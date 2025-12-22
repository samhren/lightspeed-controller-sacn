import sys
import math

def generate_icon(size=1024):
    try:
        from PIL import Image, ImageDraw
    except ImportError:
        print("Pillow not installed, skipping icon generation")
        return

    # Create a new image with a dark blue background
    img = Image.new('RGBA', (size, size), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)

    # MacOS Squircle shape (approximate)
    rect = [0, 0, size, size]
    # Draw rounded rectangle (we'll just fill most of it and let macOS handle masking usually, 
    # but for a custom icon usually we want full bleed or shaped. 
    # Let's simple fill a circle/squircle.
    
    # Background
    bg_color = (20, 20, 40, 255)
    
    # Draw a rounded rect
    corner_radius = size // 4
    draw.rounded_rectangle(rect, fill=bg_color, radius=corner_radius)

    # Draw a "Lightspeed" effect (beam)
    # Cyan to Purple gradient-ish lines
    for i in range(20):
        y = size // 2 + (i - 10) * (size // 40)
        alpha = 255 - abs(i - 10) * 20
        if alpha < 0: alpha = 0
        
        start = (0, y + i*5)
        end = (size, y - i*5)
        
        draw.line([start, end], fill=(0, 255, 255, alpha), width=size//50)
        
    # Save
    img.save("generated_icon.png")
    print("generated_icon.png created")

if __name__ == "__main__":
    generate_icon()
