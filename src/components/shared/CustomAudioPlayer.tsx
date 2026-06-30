import React, { useState, useEffect, useRef, useId } from "react";
import { FaPlay, FaPause, FaVolumeUp, FaVolumeMute, FaEllipsisV } from "react-icons/fa";
import styles from "./CustomAudioPlayer.module.css";

interface CustomAudioPlayerProps {
  src: string; // Base64 data URL or standard URL
  autoPlay?: boolean;
  wemId?: string | number; // To seed the waveform generator
}

const formatTime = (secs: number) => {
  if (isNaN(secs) || !isFinite(secs)) return "0:00";
  const mins = Math.floor(secs / 60);
  const remainderSecs = Math.floor(secs % 60);
  return `${mins}:${remainderSecs.toString().padStart(2, "0")}`;
};

const generateWaveform = (seedStr: string | number | undefined, count: number = 85) => {
  let seed = 12345;
  if (typeof seedStr === "number") {
    seed = seedStr;
  } else if (typeof seedStr === "string") {
    let hash = 0;
    for (let i = 0; i < seedStr.length; i++) {
      hash = (hash << 5) - hash + seedStr.charCodeAt(i);
      hash |= 0;
    }
    seed = Math.abs(hash);
  }

  const heights: number[] = [];
  let val = seed;
  for (let i = 0; i < count; i++) {
    val = (val * 1103515245 + 12345) & 0x7fffffff;
    const rand = (val % 38) + 12; // height 12 to 50 (viewBox height is 60)
    heights.push(rand);
  }
  return heights;
};

export default function CustomAudioPlayer({
  src,
  autoPlay = false,
  wemId,
}: CustomAudioPlayerProps) {
  const audioRef = useRef<HTMLAudioElement | null>(null);
  const [isPlaying, setIsPlaying] = useState(false);
  const [currentTime, setCurrentTime] = useState(0);
  const [duration, setDuration] = useState(0);
  const [volume, setVolume] = useState(() => {
    try {
      const saved = localStorage.getItem("omnitool-audio-volume");
      return saved !== null ? parseFloat(saved) : 0.8;
    } catch (e) {
      return 0.8;
    }
  });
  const [isMuted, setIsMuted] = useState(() => {
    try {
      const saved = localStorage.getItem("omnitool-audio-muted");
      return saved === "true";
    } catch (e) {
      return false;
    }
  });
  const [playbackRate, setPlaybackRate] = useState(1.0);
  const [isLooping, setIsLooping] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  
  const menuRef = useRef<HTMLDivElement | null>(null);
  const gradientId = useId().replace(/:/g, "-");

  const [waveformHeights, setWaveformHeights] = useState<number[]>(() =>
    generateWaveform(wemId || src)
  );

  // Decode the audio source to generate an accurate waveform
  useEffect(() => {
    let active = true;

    // First reset to LCG generator while loading/decoding the new source
    setWaveformHeights(generateWaveform(wemId || src));

    if (!src) return;

    const loadAndDecode = async () => {
      try {
        let arrayBuffer: ArrayBuffer;
        if (src.startsWith("data:")) {
          try {
            const res = await fetch(src);
            arrayBuffer = await res.arrayBuffer();
          } catch (e) {
            // fallback base64 decoding if fetch fails
            const base64Data = src.split(",")[1] || src;
            const binaryString = window.atob(base64Data);
            const len = binaryString.length;
            const bytes = new Uint8Array(len);
            for (let i = 0; i < len; i++) {
              bytes[i] = binaryString.charCodeAt(i);
            }
            arrayBuffer = bytes.buffer;
          }
        } else {
          const res = await fetch(src);
          arrayBuffer = await res.arrayBuffer();
        }

        if (!active) return;

        const AudioCtxClass = window.OfflineAudioContext || (window as any).webkitOfflineAudioContext;
        if (!AudioCtxClass) {
          throw new Error("Web Audio API not supported");
        }
        
        // Use a dummy offline context to decode the data
        const audioCtx = new AudioCtxClass(1, 1, 44100);
        const audioBuffer = await audioCtx.decodeAudioData(arrayBuffer);
        
        if (!active) return;

        const channelData = audioBuffer.getChannelData(0);
        const count = 85;
        const blockSize = Math.floor(channelData.length / count);
        const heights: number[] = [];

        // If the audio file is shorter than 85 samples, handle it gracefully
        const actualBlockSize = blockSize > 0 ? blockSize : 1;

        for (let i = 0; i < count; i++) {
          const start = i * actualBlockSize;
          const end = Math.min(start + actualBlockSize, channelData.length);
          if (start >= channelData.length) {
            heights.push(0);
            continue;
          }
          let max = 0;
          for (let j = start; j < end; j++) {
            const val = Math.abs(channelData[j]);
            if (val > max) {
              max = val;
            }
          }
          heights.push(max);
        }

        // Normalize the peak values
        let maxVal = Math.max(...heights);
        if (maxVal === 0) maxVal = 1;
        const normalizedHeights = heights.map((h) => {
          const normalized = h / maxVal;
          // FL Studio shows quiet segments as silent (thin lines in the center).
          // 4 is the minimum height (1px line/thin bar in center), 50 is the maximum height.
          return 4 + normalized * 46;
        });

        if (active) {
          setWaveformHeights(normalizedHeights);
        }
      } catch (err) {
        console.warn("Failed to decode audio waveform:", err);
        // Fallback already set above
      }
    };

    loadAndDecode();

    return () => {
      active = false;
    };
  }, [src, wemId]);

  useEffect(() => {
    // Reset state when src changes
    setIsPlaying(false);
    setCurrentTime(0);
    setDuration(0);
    if (audioRef.current) {
      audioRef.current.currentTime = 0;
      audioRef.current.playbackRate = playbackRate;
      audioRef.current.loop = isLooping;
      if (autoPlay) {
        audioRef.current.play().then(() => setIsPlaying(true)).catch(() => {});
      }
    }
  }, [src]);

  // Keep localStorage in sync with volume state
  useEffect(() => {
    try {
      localStorage.setItem("omnitool-audio-volume", volume.toString());
    } catch (e) {
      console.warn("Failed to save volume to localStorage:", e);
    }
  }, [volume]);

  // Keep localStorage in sync with isMuted state
  useEffect(() => {
    try {
      localStorage.setItem("omnitool-audio-muted", isMuted.toString());
    } catch (e) {
      console.warn("Failed to save muted state to localStorage:", e);
    }
  }, [isMuted]);

  // Sync volume and isMuted states to the HTML5 audio element
  useEffect(() => {
    if (audioRef.current) {
      audioRef.current.volume = volume;
      audioRef.current.muted = isMuted;
    }
  }, [volume, isMuted, src]);

  // Click outside menu closer
  useEffect(() => {
    const handleClickOutside = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        setMenuOpen(false);
      }
    };
    if (menuOpen) {
      document.addEventListener("mousedown", handleClickOutside);
    }
    return () => {
      document.removeEventListener("mousedown", handleClickOutside);
    };
  }, [menuOpen]);

  const handlePlayPause = () => {
    if (!audioRef.current) return;
    if (isPlaying) {
      audioRef.current.pause();
      setIsPlaying(false);
    } else {
      audioRef.current.play().then(() => setIsPlaying(true)).catch(() => {});
    }
  };

  const handleTimeUpdate = () => {
    if (audioRef.current) {
      setCurrentTime(audioRef.current.currentTime);
    }
  };

  const handleLoadedMetadata = () => {
    if (audioRef.current) {
      setDuration(audioRef.current.duration);
    }
  };

  const handleEnded = () => {
    setIsPlaying(false);
    setCurrentTime(0);
  };

  const handleVolumeChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const val = parseFloat(e.target.value);
    setVolume(val);
    if (audioRef.current) {
      audioRef.current.volume = val;
      audioRef.current.muted = val === 0;
    }
    if (val > 0 && isMuted) {
      setIsMuted(false);
    }
  };

  const toggleMute = () => {
    const nextMute = !isMuted;
    setIsMuted(nextMute);
    if (audioRef.current) {
      audioRef.current.muted = nextMute;
    }
  };

  const handlePlaybackSpeedChange = (speed: number) => {
    setPlaybackRate(speed);
    if (audioRef.current) {
      audioRef.current.playbackRate = speed;
    }
    setMenuOpen(false);
  };

  const toggleLoop = () => {
    const nextLoop = !isLooping;
    setIsLooping(nextLoop);
    if (audioRef.current) {
      audioRef.current.loop = nextLoop;
    }
    setMenuOpen(false);
  };

  const handleScrub = (clientX: number, rect: DOMRect) => {
    if (!audioRef.current || duration === 0) return;
    const clickX = clientX - rect.left;
    const percentage = Math.max(0, Math.min(1, clickX / rect.width));
    const newTime = percentage * duration;
    audioRef.current.currentTime = newTime;
    setCurrentTime(newTime);
  };

  const handleWaveformMouseDown = (e: React.MouseEvent<HTMLDivElement>) => {
    const rect = e.currentTarget.getBoundingClientRect();
    
    const handleMouseMove = (moveEvent: MouseEvent) => {
      handleScrub(moveEvent.clientX, rect);
    };

    const handleMouseUp = () => {
      window.removeEventListener("mousemove", handleMouseMove);
      window.removeEventListener("mouseup", handleMouseUp);
    };

    handleScrub(e.clientX, rect);
    window.addEventListener("mousemove", handleMouseMove);
    window.addEventListener("mouseup", handleMouseUp);
  };

  const progressPercent = duration > 0 ? (currentTime / duration) * 100 : 0;

  // Build SVG bars
  const barWidth = 3;
  const barGap = 2;
  const totalBarWidth = barWidth + barGap;
  const svgWidth = waveformHeights.length * totalBarWidth - barGap;

  const rects = waveformHeights.map((height, index) => {
    const x = index * totalBarWidth;
    const y = 30 - height / 2; // Center the bars vertically (viewBox height is 60)
    return (
      <rect
        key={index}
        x={x}
        y={y}
        width={barWidth}
        height={height}
        rx={1.5}
        ry={1.5}
      />
    );
  });

  return (
    <div className={styles.audioPlayer}>
      <audio
        ref={audioRef}
        src={src}
        onTimeUpdate={handleTimeUpdate}
        onLoadedMetadata={handleLoadedMetadata}
        onEnded={handleEnded}
      />

      {/* Top Display: Time, Waveform, Duration */}
      <div className={styles.topSection}>
        <div className={styles.waveformContainer} onMouseDown={handleWaveformMouseDown}>
          <div
            className={styles.playedGlow}
            style={{ width: `${progressPercent}%` }}
          />
          <svg
            viewBox={`0 0 ${svgWidth} 60`}
            className={styles.waveformSvg}
            preserveAspectRatio="none"
          >
            <defs>
              <linearGradient
                id={gradientId}
                x1="0"
                y1="0"
                x2={svgWidth}
                y2="0"
                gradientUnits="userSpaceOnUse"
              >
                <stop offset={`${progressPercent}%`} stopColor="var(--wave-played, #4d7cff)" />
                <stop offset={`${progressPercent}%`} stopColor="var(--wave-unplayed, #4b4b52)" />
              </linearGradient>
            </defs>
            <g fill={`url(#${gradientId})`}>
              {rects}
            </g>
          </svg>

          {/* Transparent capsule overlay for the seek handle */}
          <div className={styles.seekHandle} style={{ left: `${progressPercent}%` }}>
            <div className={styles.seekHandleLine} />
          </div>
        </div>

        <div className={`${styles.timeLabel} ${styles.currentTime}`}>{formatTime(currentTime)}</div>
        <div className={`${styles.timeLabel} ${styles.duration}`}>{formatTime(duration)}</div>
      </div>

      {/* Bottom Display: Playback Buttons & Utilities */}
      <div className={styles.bottomSection}>
        <div className={styles.playbackControls}>
          <button
            onClick={handlePlayPause}
            className={`${styles.ctrlBtn} ${isPlaying ? styles.isPlaying : ""}`}
            title={isPlaying ? "Pause" : "Play"}
          >
            {isPlaying ? <FaPause size={12} /> : <FaPlay size={12} style={{ marginLeft: "2px" }} />}
          </button>
        </div>

        <div className={styles.rightControls}>
          <div className={styles.volumeContainer}>
            <button onClick={toggleMute} className={styles.volumeBtn} title={isMuted ? "Unmute" : "Mute"}>
              {isMuted || volume === 0 ? (
                <FaVolumeMute size={16} />
              ) : (
                <FaVolumeUp size={16} />
              )}
            </button>
            <input
              type="range"
              min="0"
              max="1"
              step="0.01"
              value={isMuted ? 0 : volume}
              onChange={handleVolumeChange}
              className={styles.volumeSlider}
              style={{
                background: `linear-gradient(to right, #ffffff ${
                  (isMuted ? 0 : volume) * 100
                }%, rgba(255,255,255,0.15) ${(isMuted ? 0 : volume) * 100}%)`,
              }}
            />
          </div>

          {/* Three-dot options menu */}
          <div className={styles.menuContainer} ref={menuRef}>
            <button
              onClick={() => setMenuOpen(!menuOpen)}
              className={styles.menuBtn}
              title="Options"
            >
              <FaEllipsisV size={16} />
            </button>

            {menuOpen && (
              <div className={styles.optionsDropdown}>
                <button onClick={toggleLoop} className={styles.dropdownItem}>
                  <span>Loop Playback</span>
                  <span className={styles.activeCheck}>{isLooping ? "✓" : ""}</span>
                </button>

                <div className={styles.dropdownSeparator} />
                <div className={styles.speedSection}>
                  <div className={styles.dropdownLabel}>Playback Speed</div>
                  <div className={styles.speedGrid}>
                    {[0.5, 1.0, 1.5, 2.0].map((speed) => (
                      <button
                        key={speed}
                        onClick={() => handlePlaybackSpeedChange(speed)}
                        className={`${styles.speedBtn} ${
                          playbackRate === speed ? styles.speedBtnActive : ""
                        }`}
                      >
                        {speed.toFixed(1)}x
                      </button>
                    ))}
                  </div>
                </div>
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
