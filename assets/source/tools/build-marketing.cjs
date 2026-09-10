// Composes the README banners and the social cards: the legacy tile and the
// outlined vector wordmark over the generated atmosphere plates in
// source/marketing. Run from this directory after build-icons.cjs.
const fs=require('fs'),path=require('path'),sharp=require('sharp');
const {continuousPath}=require('./surface.cjs');
const root=path.resolve(__dirname,'..','..'),read=p=>fs.readFileSync(path.join(root,p));
function put(p,b){fs.mkdirSync(path.dirname(path.join(root,p)),{recursive:true});fs.writeFileSync(path.join(root,p),b);}
const word=JSON.parse(read('source/marketing/wordmark-paths.json'));
const svg=(b,w,h)=>Buffer.from(`<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}" viewBox="0 0 ${w} ${h}">${b}</svg>`);
async function render(theme,W,H,isBanner){
 const dark=theme==='dark',f=W/(isBanner?1800:1280);
 const size=(isBanner?356:262)*f,x=(isBanner?304:210)*f,y=(H-size)/2-8*f;
 const tx=x+size+60*f,tw=(isBanner?760:545)*f,th=tw/word.width*word.height,ty=(H-th)/2+8*f;
 const scale=tw/word.width;
 const paths=word.glyphs.map(g=>`<path d="${g.d}" stroke="inherit" stroke-width="18" stroke-linejoin="round"/>`).join('');
 const glyph=`<g transform="translate(${tx} ${ty}) scale(${scale}) translate(${-word.bounds[0]} ${-word.bounds[1]})">${paths}</g>`;
 const defs=`<defs><linearGradient id="ink" x1="0" y1="0" x2="0" y2="1"><stop stop-color="${dark?'#FFFFFF':'#57595D'}"/><stop offset=".22" stop-color="${dark?'#F8FAFC':'#3B3D40'}"/><stop offset=".8" stop-color="${dark?'#E5E8EC':'#202124'}"/><stop offset="1" stop-color="${dark?'#F7F6F3':'#303033'}"/></linearGradient></defs>`;
 const mask=await sharp(svg(`<g fill="white" stroke="white">${glyph}</g>`,W,H)).png().toBuffer();
 const alpha=await sharp(mask).extractChannel('alpha').toBuffer();
 const tint=async(color,opacity=1)=>{const a=await sharp(alpha).linear(opacity).toBuffer();return sharp({create:{width:W,height:H,channels:3,background:color}}).joinChannel(a).png().toBuffer();};
 const wordArt=await sharp(svg(`${defs}<g fill="url(#ink)" stroke="url(#ink)">${glyph}</g>`,W,H)).png().toBuffer();
 // One-pixel inner highlights are bounded by the real glyph outlines.
 const shift=async(dy)=>sharp(svg(`<g fill="white" stroke="white" transform="translate(0 ${dy})">${glyph}</g>`,W,H)).png().toBuffer();
 const top=await sharp(mask).composite([{input:await shift(1.6*f),blend:'dest-out'}]).png().toBuffer();
 const bottom=await sharp(mask).composite([{input:await shift(-1.8*f),blend:'dest-out'}]).png().toBuffer();
 const tintEdge=async(src,color,strength)=>{const a=await sharp(src).extractChannel('alpha').linear(strength).toBuffer();return sharp({create:{width:W,height:H,channels:3,background:color}}).joinChannel(a).png().toBuffer();};
 const wordShadow=await sharp(await tint('#000000',dark?.32:.18)).blur(Math.max(.3,6*f)).affine([[1,0],[0,1]],{ody:4*f,background:'#00000000'}).resize(W,H,{fit:'fill'}).png().toBuffer();
 const tilePath=continuousPath(x,y,size);
 const shadows=svg(`<defs><filter id="ambient" x="-50%" y="-50%" width="200%" height="200%"><feGaussianBlur stdDeviation="${18*f}"/></filter><filter id="contact" x="-50%" y="-50%" width="200%" height="200%"><feGaussianBlur stdDeviation="${5*f}"/></filter></defs><path d="${tilePath}" transform="translate(0 ${14*f})" fill="${dark?'#000000':'#504239'}" opacity="${dark?'.34':'.16'}" filter="url(#ambient)"/><path d="${tilePath}" transform="translate(0 ${4*f})" fill="#000000" opacity="${dark?'.22':'.08'}" filter="url(#contact)"/>`,W,H);
 const tile=await sharp(read('macos/Vapor.iconset/icon_512x512@2x.png')).extract({left:100,top:100,width:824,height:824}).resize(Math.round(size),Math.round(size)).png().toBuffer();
 let backdrop=await sharp(read(`source/marketing/atmosphere-${theme}.png`)).resize(W,H,{fit:'fill'}).png().toBuffer();
 if(dark)backdrop=await sharp(backdrop).composite([{input:svg(`<rect width="${W}" height="${H}" fill="#0D1014" opacity=".4"/>`,W,H)}]).png().toBuffer();
 return sharp(backdrop).composite([{input:shadows},{input:tile,left:Math.round(x),top:Math.round(y)},{input:wordShadow},{input:wordArt},{input:await tintEdge(top,'#FFFFFF',dark?.85:.42)},{input:await tintEdge(bottom,dark?'#989DA5':'#0A0A0B',.5)}]).png().toBuffer();
}
(async()=>{
 for(const theme of ['light','dark']){
  put(`github/vapor-banner-${theme}.png`,await render(theme,1800,600,true));
  for(const [kind,w,h,dest]of [['github',1280,640,`github/vapor-social-${theme}-1280x640.jpg`],['og',1200,630,`web/public/social/vapor-og-${theme}-1200x630.jpg`],['card',1200,600,`web/public/social/vapor-card-${theme}-1200x600.jpg`]])put(dest,await sharp(await render(theme,w,h,false)).jpeg({quality:94,chromaSubsampling:'4:4:4',mozjpeg:true}).toBuffer());
 }
 console.log('Rebuilt both GitHub banners and all six social previews from clean outlines.');
})();
