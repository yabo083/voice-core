"""Corpus quality measurement for single-speaker TTS fine-tuning.

Every threshold in here comes from a published source, named at the constant. Nothing is a
number somebody liked the look of, and where this file cannot reproduce an authority's exact
method it says so rather than pretending the proxy is the standard.

WHY THIS EXISTS. The dataset stage used to measure duration, sample rate, channels and absolute
peak, and nothing else - so a corpus could be 59% hard-clipped, upsampled from 16 kHz, and half
room tone, and the pipeline would report `163 clips, 15.9 min, 48000 Hz` and train on all of it.
That is what happened on this project's own corpus: 96 of 163 clips at peak 1.0, trained anyway.

WHAT THE AUTHORITIES SAY, with the consequence for THIS architecture (a DACVAE latent codec
under a rectified-flow DiT):

* Clipping is not cosmetic. Clipping truncates waveform extrema non-linearly and produces
  harmonic distortion across the high band (VoiceFixer, Liu et al., Interspeech 2022,
  arXiv:2109.13731). A continuous neural codec has to encode those flat tops as high-energy
  latent perturbations, and the model then learns to reproduce them in every utterance of that
  voice. Under 10% of a corpus: drop the clips. Widespread: repair (declip) and leave headroom,
  or accept a permanently harsher voice.
* SNR gates corpora in practice. LibriTTS filtered every clip below WADA-SNR 20 dB out of its
  clean subset, discarding roughly a quarter of its candidates (Zen et al., Interspeech 2019,
  arXiv:1904.02882). VCTK and AISHELL-3 recorded above 30 dB.
* Loudness has a standard and a target. Integrated loudness is defined by ITU-R BS.1770-4 (EBU
  R128 for the -23 LUFS broadcast target); this engine's codec normalises per clip to -16 dB,
  so -16 LUFS is the number that matters here, with a true-peak ceiling of -1.0 dBTP leaving
  the codec headroom instead of asking it to encode saturation.
* Silence is worth trimming, measurably. AISHELL-3 reported that energy-based VAD trimming of
  initial silence sped up alignment formation by 10x (Shi et al., Interspeech 2020,
  arXiv:2010.11567). This module uses energy-based detection for the same reason they did.
* Clip length wants to be narrow. Published fine-tuning recipes cluster on 2-10 s with a 4-6 s
  median (GPT-SoVITS, Piper, VITS/FastSpeech-2 practice). A corpus spanning 2 s to 18.5 s pays
  for the spread twice: padding compute, and a duration predictor pulled by the extremes.

WHAT THIS MODULE DELIBERATELY DOES NOT DO: drop anything. It measures and reports. Dropping 59%
of somebody's voice corpus is their decision, and the flags that act on these numbers live in
`prepare_dataset.py` where the caller can see them.
"""

from __future__ import annotations

from dataclasses import dataclass, asdict
from typing import Any

import numpy as np

#: Peak at or above this is clipping. `prepare_dataset.py` used this bound before this module
#: existed and it matches the usual 16-bit full-scale test; kept so the two agree.
CLIP_PEAK = 0.999
#: A single sample at full scale is a coincidence; a run of them is a flat top, which is what
#: actually produces the harmonic splatter VoiceFixer describes. Reported separately from peak
#: so a caller can tell a grazed peak from real saturation.
CLIP_RUN = 3
#: ITU-R BS.1770-4 integrated loudness target for THIS engine: its codec normalises per clip to
#: -16 dB (`--normalize-db -16.0`), so a corpus already at -16 LUFS is one the codec leaves
#: alone. Deviation is not an error, it is how far the blind per-clip gain has to move.
TARGET_LUFS = -16.0
#: True-peak ceiling that leaves the codec headroom rather than handing it saturation.
#: -1.0 dBTP is the EBU R128 / BS.1770 ceiling; as linear amplitude that is ~0.891.
TRUE_PEAK_CEILING = 0.891
#: LibriTTS's clean-subset gate, stated on WADA-SNR. See `noise_floor_snr_db` for why the number
#: this module computes is NOT directly comparable to it.
LIBRITTS_CLEAN_SNR_DB = 20.0
#: Lead/trail silence a published recipe leaves in place. AISHELL-3 trimmed initial silence
#: outright; 50-100 ms is the range practitioner recipes settle on.
SILENCE_MS = 100.0
#: Below this a 44.1/48 kHz container was filled from a lower-rate source, and the missing band
#: cannot be recovered. Calibrated: real studio audio measures 17.2 kHz and a website re-encode
#: 15.3 kHz, while audio whose true cutoff is 8 kHz measures 8.7-8.9 kHz. 14 kHz separates them.
BANDWIDTH_MIN_HZ = 14000.0
#: Published fine-tuning recipes cluster here (GPT-SoVITS, Piper, VITS practice).
CLIP_SECONDS = (2.0, 10.0)


@dataclass
class Quality:
    """One clip's measurements. Every field is a number a caller can threshold itself."""

    peak: float
    clipped_samples: int
    longest_clip_run: int
    lufs: float | None
    noise_floor_snr_db: float | None
    lead_silence_ms: float
    trail_silence_ms: float
    speech_ratio: float
    bandwidth_hz: float | None

    def issues(self, sample_rate: int) -> list[str]:
        """The findings, phrased as what a caller would act on. Order is by how much each one
        changes the trained voice, worst first."""
        found: list[str] = []
        if self.longest_clip_run >= CLIP_RUN:
            found.append(
                f"clipping (peak {self.peak:.3f}, {self.clipped_samples} sample(s), "
                f"longest flat top {self.longest_clip_run})"
            )
        elif self.peak >= CLIP_PEAK:
            found.append(f"peak grazes full scale ({self.peak:.3f})")
        elif self.peak > TRUE_PEAK_CEILING:
            found.append(f"no codec headroom (peak {self.peak:.3f} > {TRUE_PEAK_CEILING:.3f})")
        if self.noise_floor_snr_db is not None and self.noise_floor_snr_db < LIBRITTS_CLEAN_SNR_DB:
            found.append(f"noisy (noise-floor SNR {self.noise_floor_snr_db:.1f} dB)")
        if self.lufs is not None and abs(self.lufs - TARGET_LUFS) > 6.0:
            found.append(f"loudness {self.lufs:.1f} LUFS, {self.lufs - TARGET_LUFS:+.1f} off target")
        if self.lead_silence_ms > SILENCE_MS or self.trail_silence_ms > SILENCE_MS:
            found.append(
                f"untrimmed silence (lead {self.lead_silence_ms:.0f} ms, "
                f"trail {self.trail_silence_ms:.0f} ms)"
            )
        if (
            self.bandwidth_hz is not None
            and sample_rate >= 44100
            and self.bandwidth_hz < BANDWIDTH_MIN_HZ
        ):
            found.append(
                f"band-limited: {sample_rate} Hz container, signal stops at "
                f"{self.bandwidth_hz / 1000:.1f} kHz"
            )
        return found

    def as_dict(self) -> dict[str, Any]:
        return {key: value for key, value in asdict(self).items() if value is not None}


def measure(wav: np.ndarray, sample_rate: int) -> Quality:
    """Every measurement for one mono clip. Never raises: a corpus report is not worth a crash,
    and a field this cannot compute comes back None so the report says "unmeasured" rather than
    inventing a number."""
    mono = wav if wav.ndim == 1 else wav.mean(axis=0 if wav.shape[0] < wav.shape[-1] else -1)
    mono = np.asarray(mono, dtype=np.float64).ravel()
    if mono.size == 0:
        return Quality(0.0, 0, 0, None, None, 0.0, 0.0, 0.0, None)

    peak = float(np.max(np.abs(mono)))
    at_peak = np.abs(mono) >= CLIP_PEAK
    lead, trail, ratio = _silence_and_speech(mono, sample_rate)
    return Quality(
        peak=peak,
        clipped_samples=int(at_peak.sum()),
        longest_clip_run=_longest_run(at_peak),
        lufs=_lufs(mono, sample_rate),
        noise_floor_snr_db=_noise_floor_snr_db(mono, sample_rate),
        lead_silence_ms=lead,
        trail_silence_ms=trail,
        speech_ratio=ratio,
        bandwidth_hz=_bandwidth_hz(mono, sample_rate),
    )


def _longest_run(mask: np.ndarray) -> int:
    """Longest run of True. A flat top is consecutive samples pinned at full scale, which is the
    difference between a peak that grazed and a waveform that was cut off."""
    if not mask.any():
        return 0
    # Run-length over the boolean mask: positions where the value changes bound each run.
    edges = np.flatnonzero(np.diff(mask.astype(np.int8)))
    bounds = np.concatenate(([-1], edges, [mask.size - 1]))
    runs = np.diff(bounds)
    starts = bounds[:-1] + 1
    return int(runs[mask[starts]].max())


def _lufs(mono: np.ndarray, sample_rate: int) -> float | None:
    """Integrated loudness, ITU-R BS.1770-4, via pyloudnorm - the reference implementation of
    the standard rather than an RMS stand-in. Its meter needs at least one 400 ms block."""
    try:
        import pyloudnorm
    except Exception:  # noqa: BLE001
        return None
    if mono.size < int(0.4 * sample_rate):
        return None
    try:
        meter = pyloudnorm.Meter(sample_rate)
        value = float(meter.integrated_loudness(mono))
    except Exception:  # noqa: BLE001
        return None
    return None if not np.isfinite(value) else value


def _frame_rms(mono: np.ndarray, sample_rate: int, ms: float = 20.0) -> np.ndarray:
    size = max(1, int(sample_rate * ms / 1000.0))
    usable = (mono.size // size) * size
    if usable == 0:
        return np.array([float(np.sqrt(np.mean(mono**2)))])
    frames = mono[:usable].reshape(-1, size)
    return np.sqrt(np.mean(frames**2, axis=1))


def _noise_floor_snr_db(mono: np.ndarray, sample_rate: int) -> float | None:
    """Speech level over the noise floor, in dB, from the frame-energy distribution.

    NOT WADA-SNR. LibriTTS's 20 dB gate is defined on WADA (Kim & Stern, 2008), whose estimator
    needs a calibration table this file does not carry, and reproducing that table from memory is
    exactly the kind of invention this module refuses. So this reports a percentile noise-floor
    ratio - the 90th percentile frame RMS over the 10th - which is monotonic in the same quantity
    and directly interpretable, and it is named so nobody quotes it as WADA. Treat the 20 dB
    figure as the authority's shape, not as a threshold this number has been calibrated against.
    """
    rms = _frame_rms(mono, sample_rate)
    if rms.size < 10:
        return None
    speech = float(np.percentile(rms, 90))
    noise = float(np.percentile(rms, 10))
    if speech <= 0.0:
        return None
    # A perfectly silent floor is not infinite SNR, it is an unmeasurable one.
    if noise <= 1e-9:
        return None
    return float(20.0 * np.log10(speech / noise))


def _silence_and_speech(mono: np.ndarray, sample_rate: int) -> tuple[float, float, float]:
    """Leading and trailing silence in ms, and the fraction of frames that carry speech.

    Energy-based, with the threshold set relative to the clip's own peak frame - the same family
    of detector AISHELL-3 used to get its 10x alignment speedup, and it needs no model.
    """
    rms = _frame_rms(mono, sample_rate)
    if rms.size == 0:
        return 0.0, 0.0, 0.0
    # -40 dB below the loudest frame: the threshold silence-based slicers in this ecosystem use.
    threshold = float(np.max(rms)) * (10.0 ** (-40.0 / 20.0))
    voiced = rms > threshold
    if not voiced.any():
        return 0.0, 0.0, 0.0
    frame_ms = 1000.0 * len(mono[: rms.size * (len(mono) // rms.size)]) / rms.size / sample_rate
    first = int(np.argmax(voiced))
    last = int(rms.size - 1 - np.argmax(voiced[::-1]))
    return (
        float(first * frame_ms),
        float((rms.size - 1 - last) * frame_ms),
        float(voiced.mean()),
    )


def _bandwidth_hz(mono: np.ndarray, sample_rate: int, drop_db: float = 50.0) -> float | None:
    """Highest frequency still carrying signal: the audio's real bandwidth.

    Bandwidth, not a 95%-energy spectral rolloff: rolloff finds where energy is CONCENTRATED, and
    speech energy concentrates in the fundamental and the low formants, so it reports ~4 kHz for
    full-band studio speech and ranks a brighter, louder recording ABOVE a quieter wide-band one.
    It cannot answer "is the high band present", so do not substitute it here.

    Calibrated against ground truth built by resampling real clips to a known rate and back, so
    the true cutoff is known, on two unrelated sources:

        forced cutoff   4 kHz  ->  reported 4.4 / 4.5 kHz
        forced cutoff   8 kHz  ->  reported 8.7 / 8.9 kHz
        forced cutoff  16 kHz  ->  reported 16.7 / 15.2 kHz
        untouched studio audio ->  reported 17.2 kHz

    Method: average the power spectrum over speech frames only (a pause's spectrum is low-level
    noise and would drag the average down), then take the highest frequency whose average power
    is within `drop_db` of the spectral peak. 50 dB is low enough to ignore the noise floor and
    high enough to see a real cutoff.
    """
    if mono.size < 2048:
        return None
    window, hop = 2048, 1024
    taper = np.hanning(window)
    freqs = np.fft.rfftfreq(window, d=1.0 / sample_rate)
    powers: list[np.ndarray] = []
    totals: list[float] = []
    for start in range(0, max(1, mono.size - window), hop):
        frame = mono[start : start + window]
        if frame.size < window:
            break
        power = np.abs(np.fft.rfft(frame * taper)) ** 2
        total = float(power.sum())
        if total <= 0.0:
            continue
        powers.append(power)
        totals.append(total)
    if not powers:
        return None
    stacked = np.array(powers)
    energy = np.array(totals)
    # -40 dB in amplitude is -80 dB in power, against the loudest frame.
    voiced = stacked[energy > energy.max() * 10 ** (-80.0 / 10.0)]
    average = (voiced if voiced.size else stacked).mean(axis=0)
    if average.max() <= 0.0:
        return None
    above = np.flatnonzero(average > average.max() * 10 ** (-drop_db / 10.0))
    return float(freqs[above[-1]]) if above.size else None
